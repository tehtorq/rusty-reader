package com.rustyreader;

import android.content.Context;
import android.content.res.AssetManager;
import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioTrack;
import android.util.Log;

import com.k2fsa.sherpa.onnx.GeneratedAudio;
import com.k2fsa.sherpa.onnx.OfflineTts;
import com.k2fsa.sherpa.onnx.OfflineTtsConfig;
import com.k2fsa.sherpa.onnx.OfflineTtsModelConfig;
import com.k2fsa.sherpa.onnx.OfflineTtsVitsModelConfig;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.concurrent.LinkedBlockingQueue;

/**
 * Piper's neural voices, run in the app by sherpa-onnx: the same voices as on the desktop, the
 * same on every phone, and offline once downloaded (each is about 60 MB, fetched the first time
 * it is used). Speaks like the system engine does for {@link Narration}: utterances are queued,
 * and each reports when it starts and finishes.
 *
 * One thread turns sentences into audio, a little ahead; another plays them in order.
 */
final class NaturalVoices {
    private static final String TAG = "RustyReader";
    private static final String MODELS = "https://huggingface.co/csukuangfj/vits-piper-";

    /** The voices offered: Piper's name for each, a label, and its language. */
    private static final String[][] CATALOGUE = {
        {"en_GB-alan-medium", "Alan (UK)", "en-GB"},
        {"en_GB-jenny_dioco-medium", "Jenny (UK)", "en-GB"},
        {"en_GB-northern_english_male-medium", "Northern English man (UK)", "en-GB"},
        {"en_GB-southern_english_female-medium", "Southern English woman (UK)", "en-GB"},
        {"en_GB-alba-medium", "Alba (Scotland)", "en-GB"},
        {"en_US-lessac-medium", "Lessac (US)", "en-US"},
        {"en_US-ryan-medium", "Ryan (US)", "en-US"},
        {"en_US-amy-medium", "Amy (US)", "en-US"},
        {"en_US-kristin-medium", "Kristin (US)", "en-US"},
        {"en_US-joe-medium", "Joe (US)", "en-US"},
    };

    private final Context context;
    private final AudioAttributes audio;

    private volatile String chosen;
    private volatile String language = "en";
    private volatile float rate = 1f;
    /** Bumped by stop(): work from before it is dropped. */
    private volatile long generation;

    private final LinkedBlockingQueue<Job> toSay = new LinkedBlockingQueue<>();
    private final LinkedBlockingQueue<Job> toPlay = new LinkedBlockingQueue<>(2);

    // Only the synthesis thread touches these.
    private OfflineTts tts;
    private String loaded;

    private final Object trackLock = new Object();
    private AudioTrack track;
    private int trackRate;
    /** The stop() generation the track was made in: a stop leaves it flushed, and done with. */
    private long trackGeneration = -1;
    private long framesWritten;
    /** Sentences written to the track and not yet heard to the end, oldest first. */
    private final List<Job> inFlight = new ArrayList<>();

    /** Pauses after a sentence, and after a paragraph, in seconds at normal speed. The voice
     *  leaves about a tenth of a second of its own at each join. */
    private static final float SENTENCE_PAUSE = 0.2f, PARAGRAPH_PAUSE = 0.55f;

    private final Object downloadLock = new Object();

    private static final class Job {
        final long id, generation;
        final String text;
        final boolean paragraphEnd;
        float[] samples;
        int sampleRate;
        /** Silence to follow it, in samples. */
        int pause;
        /** Where it begins and ends in the track, in samples, once written. */
        long start, end;
        boolean started;

        Job(long id, String text, boolean paragraphEnd, long generation) {
            this.id = id;
            this.text = text;
            this.paragraphEnd = paragraphEnd;
            this.generation = generation;
        }
    }

    NaturalVoices(Context context, AudioAttributes audio) {
        this.context = context.getApplicationContext();
        this.audio = audio;
        Thread synth = new Thread(this::synthesise, "natural-voice-synth");
        synth.setDaemon(true);
        synth.start();
        Thread play = new Thread(this::play, "natural-voice-play");
        play.setDaemon(true);
        play.start();
        Thread watch = new Thread(this::watch, "natural-voice-watch");
        watch.setDaemon(true);
        watch.start();
    }

    // ---- What Narration asks of a voice --------------------------------------------------

    void speak(long id, String text, boolean paragraphEnd) {
        toSay.add(new Job(id, text, paragraphEnd, generation));
    }

    void stop() {
        generation++;
        toSay.clear();
        toPlay.clear();
        // Silence now; the playing thread replaces the track before its next sentence.
        synchronized (trackLock) {
            if (track != null) {
                track.pause();
                track.flush();
            }
            inFlight.clear();
        }
    }

    void rate(float rate) {
        this.rate = rate;
    }

    void language(String tag) {
        language = tag;
    }

    /**
     * Choose a voice (null: the first for the book's language). One chosen by name is fetched
     * straight away, ready for when it is needed; otherwise a voice is fetched when first used.
     */
    void voice(String name) {
        chosen = name;
        String voice = current();
        if (name != null && !downloaded(voice)) {
            new Thread(() -> download(voice), "natural-voice-download").start();
        }
    }

    String current() {
        if (chosen != null) {
            for (String[] v : CATALOGUE) if (v[0].equals(chosen)) return chosen;
        }
        String lang = language.toLowerCase(Locale.ROOT).replace('_', '-');
        // A downloaded voice in the book's language, else the first in it, else Alan.
        for (String[] v : CATALOGUE) if (lang.startsWith(v[2].toLowerCase(Locale.ROOT)) && downloaded(v[0])) return v[0];
        for (String[] v : CATALOGUE) if (lang.startsWith(v[2].toLowerCase(Locale.ROOT))) return v[0];
        for (String[] v : CATALOGUE) if (downloaded(v[0])) return v[0];
        return CATALOGUE[0][0];
    }

    /** All the voices, as "name\tlanguage tag\tlabel"; those not yet downloaded say how big. */
    String[] voices() {
        List<String> out = new ArrayList<>();
        for (String[] v : CATALOGUE) {
            String label = downloaded(v[0]) ? v[1] : v[1] + " · 60 MB";
            out.add(v[0] + "\t" + v[2] + "\t" + label);
        }
        return out.toArray(new String[0]);
    }

    // ---- Making speech ----------------------------------------------------------------

    private void synthesise() {
        while (true) {
            Job job;
            try {
                job = toSay.take();
            } catch (InterruptedException e) {
                return;
            }
            if (job.generation != generation) continue;
            try {
                String voice = current();
                if (!downloaded(voice) && !download(voice)) {
                    Narration.heard(Narration.FAILED, job.id);
                    continue;
                }
                if (!voice.equals(loaded)) load(voice);
                if (job.generation != generation) continue;
                GeneratedAudio speech = tts.generate(job.text, 0, rate);
                job.samples = speech.getSamples();
                job.sampleRate = speech.getSampleRate();
                float pause = job.paragraphEnd ? PARAGRAPH_PAUSE : SENTENCE_PAUSE;
                job.pause = (int) (pause / Math.max(0.25f, rate) * job.sampleRate);
                if (job.generation == generation) toPlay.put(job);
            } catch (InterruptedException e) {
                return;
            } catch (Throwable e) {
                // Anything at all (a missing library, running out of memory): stop reading and
                // say why, rather than take the app down.
                fail("making speech", job, e);
            }
        }
    }

    private void load(String voice) throws IOException {
        if (tts != null) tts.release();
        tts = null;
        loaded = null;
        File dir = voiceDir(voice);
        OfflineTtsVitsModelConfig vits = OfflineTtsVitsModelConfig.builder()
                .setModel(new File(dir, "model.onnx").getPath())
                .setTokens(new File(dir, "tokens.txt").getPath())
                .setDataDir(espeakData().getPath())
                .build();
        OfflineTtsModelConfig model = OfflineTtsModelConfig.builder()
                .setVits(vits)
                .setNumThreads(Math.max(1, Math.min(4, Runtime.getRuntime().availableProcessors() / 2)))
                .setDebug(false)
                .build();
        tts = new OfflineTts(OfflineTtsConfig.builder().setModel(model).build());
        loaded = voice;
    }

    // ---- Playing it -------------------------------------------------------------------
    //
    // Sentences go into the track back to back, each followed by its pause, so the voice runs
    // on like someone reading aloud. A watcher follows the track's playback position to report
    // each sentence as it is actually heard starting and finishing.

    private void play() {
        while (true) {
            Job job;
            try {
                job = toPlay.take();
            } catch (InterruptedException e) {
                return;
            }
            if (job.generation != generation) continue;
            try {
                write(job);
            } catch (Throwable e) {
                fail("playing speech", job, e);
            }
        }
    }

    private static void fail(String doing, Job job, Throwable e) {
        Log.e(TAG, "natural voice failed " + doing, e);
        Narration.failure("Natural voice failed " + doing + ": " + e);
        Narration.heard(Narration.FAILED, job.id);
    }

    /** Queue a sentence, and its pause, behind whatever is playing. */
    private void write(Job job) {
        AudioTrack out;
        synchronized (trackLock) {
            if (job.generation != generation) return;
            if (track == null || trackRate != job.sampleRate || trackGeneration != job.generation) {
                if (track != null) track.release();
                int min = AudioTrack.getMinBufferSize(job.sampleRate, AudioFormat.CHANNEL_OUT_MONO,
                        AudioFormat.ENCODING_PCM_FLOAT);
                track = new AudioTrack.Builder()
                        .setAudioAttributes(audio)
                        .setAudioFormat(new AudioFormat.Builder()
                                .setEncoding(AudioFormat.ENCODING_PCM_FLOAT)
                                .setSampleRate(job.sampleRate)
                                .setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
                                .build())
                        .setTransferMode(AudioTrack.MODE_STREAM)
                        // Half a second, in whole samples (4 bytes each): Android refuses a
                        // buffer that would end part-way through one.
                        .setBufferSizeInBytes(Math.max(min, job.sampleRate / 2 * 4))
                        .build();
                if (android.os.Build.VERSION.SDK_INT >= 31) {
                    // Start as soon as a twentieth of a second is in, not once the buffer is full.
                    track.setStartThresholdInFrames(job.sampleRate / 20);
                }
                trackRate = job.sampleRate;
                trackGeneration = job.generation;
                framesWritten = 0;
                inFlight.clear();
                track.play();
            }
            out = track;
            job.start = framesWritten;
            job.end = framesWritten + job.samples.length + job.pause;
            framesWritten = job.end;
            inFlight.add(job);
        }
        // Write in pieces, so a stop takes effect at once. Writing blocks while the track's
        // buffer is full, which keeps this thread just ahead of what is being heard.
        int chunk = job.sampleRate / 10;
        for (int at = 0; at < job.samples.length; at += chunk) {
            if (job.generation != generation) return;
            int n = Math.min(chunk, job.samples.length - at);
            if (out.write(job.samples, at, n, AudioTrack.WRITE_BLOCKING) < 0) return;
        }
        float[] silence = new float[Math.min(chunk, Math.max(job.pause, 1))];
        for (int left = job.pause; left > 0; left -= silence.length) {
            if (job.generation != generation) return;
            if (out.write(silence, 0, Math.min(left, silence.length), AudioTrack.WRITE_BLOCKING) < 0) return;
        }
    }

    /** Report sentences as the track reaches them: started, then (pause included) done. */
    private void watch() {
        while (true) {
            try {
                Thread.sleep(15);
            } catch (InterruptedException e) {
                return;
            }
            List<long[]> events = new ArrayList<>();
            synchronized (trackLock) {
                if (track == null || inFlight.isEmpty()) continue;
                long head;
                try {
                    head = track.getPlaybackHeadPosition() & 0xffffffffL;
                } catch (IllegalStateException e) {
                    continue;
                }
                while (!inFlight.isEmpty()) {
                    Job job = inFlight.get(0);
                    if (job.generation != generation) {
                        inFlight.remove(0);
                        continue;
                    }
                    if (!job.started && head >= job.start) {
                        job.started = true;
                        events.add(new long[] {Narration.STARTED, job.id});
                    }
                    if (head < job.end) break;
                    events.add(new long[] {Narration.DONE, job.id});
                    inFlight.remove(0);
                }
            }
            for (long[] event : events) Narration.heard((int) event[0], event[1]);
        }
    }

    // ---- Files ------------------------------------------------------------------------

    private File voiceDir(String voice) {
        return new File(context.getFilesDir(), "voices/" + voice);
    }

    private boolean downloaded(String voice) {
        File dir = voiceDir(voice);
        return new File(dir, "model.onnx").isFile() && new File(dir, "tokens.txt").isFile();
    }

    /** Fetch a voice (once, however many ask), reporting progress to the reader. */
    private boolean download(String voice) {
        synchronized (downloadLock) {
            if (downloaded(voice)) return true;
            File dir = voiceDir(voice);
            dir.mkdirs();
            try {
                String base = MODELS + voice + "/resolve/main/";
                fetch(base + "tokens.txt", new File(dir, "tokens.txt"), null);
                fetch(base + voice + ".onnx", new File(dir, "model.onnx"), voice);
                Narration.progress(voice, 1f);
                return true;
            } catch (IOException e) {
                Log.w(TAG, "could not download " + voice, e);
                Narration.progress(voice, -1f);
                return false;
            }
        }
    }

    private static void fetch(String url, File to, String progressFor) throws IOException {
        File part = new File(to.getPath() + ".part");
        HttpURLConnection connection = (HttpURLConnection) new URL(url).openConnection();
        connection.setInstanceFollowRedirects(true);
        connection.setConnectTimeout(15000);
        connection.setReadTimeout(30000);
        try (InputStream in = connection.getInputStream(); OutputStream out = new FileOutputStream(part)) {
            long total = connection.getContentLengthLong();
            long done = 0;
            int lastPercent = -1;
            byte[] buffer = new byte[64 * 1024];
            for (int n; (n = in.read(buffer)) > 0; ) {
                out.write(buffer, 0, n);
                done += n;
                int percent = total > 0 ? (int) (done * 100 / total) : 0;
                if (progressFor != null && percent != lastPercent) {
                    lastPercent = percent;
                    Narration.progress(progressFor, Math.min(0.99f, percent / 100f));
                }
            }
        } finally {
            connection.disconnect();
        }
        if (!part.renameTo(to)) throw new IOException("could not save " + to);
    }

    /** espeak-ng's pronunciation data, shipped in the app and unpacked on first use. */
    private File espeakData() throws IOException {
        File dir = new File(context.getFilesDir(), "espeak-ng-data");
        File done = new File(dir, ".unpacked");
        if (!done.isFile()) {
            copyAssets(context.getAssets(), "espeak-ng-data", dir);
            done.createNewFile();
        }
        return dir;
    }

    private static void copyAssets(AssetManager assets, String path, File to) throws IOException {
        String[] children = assets.list(path);
        if (children == null || children.length == 0) {
            to.getParentFile().mkdirs();
            try (InputStream in = assets.open(path); OutputStream out = new FileOutputStream(to)) {
                byte[] buffer = new byte[64 * 1024];
                for (int n; (n = in.read(buffer)) > 0; ) out.write(buffer, 0, n);
            }
            return;
        }
        to.mkdirs();
        for (String child : children) copyAssets(assets, path + "/" + child, new File(to, child));
    }
}
