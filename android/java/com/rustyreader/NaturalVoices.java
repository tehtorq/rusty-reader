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
import com.k2fsa.sherpa.onnx.OfflineTtsKokoroModelConfig;
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
 * Neural voices, run in the app by sherpa-onnx, offline once downloaded: Piper's (the same as
 * on the desktop; about 60 MB each) and Kokoro's (more natural still, but heavier: one 360 MB
 * model holds all of its voices). Voices are downloaded from the voice list, or the first time
 * one is asked to speak. Speaks like the system engine does for {@link Narration}: utterances
 * are queued, and each reports when it starts and finishes.
 *
 * One thread turns sentences into audio, a little ahead; another plays them in order.
 */
final class NaturalVoices {
    private static final String TAG = "RustyReader";
    private static final String MODELS = "https://huggingface.co/csukuangfj/vits-piper-";
    private static final String KOKORO = "https://huggingface.co/csukuangfj/kokoro-multi-lang-v1_0/resolve/main/";
    /** Kokoro's voices share one model, kept in this folder. */
    private static final String KOKORO_DIR = "kokoro-multi-lang-v1_0";
    private static final String KOKORO_PREFIX = "kokoro-";

    /**
     * The voices offered: an id, a label, and its language. Piper voices are named as Piper
     * names them; Kokoro's are "kokoro-" and the voice's name in the model, whose speaker number
     * is the last field.
     */
    private static final String[][] CATALOGUE = {
        {"kokoro-af_heart", "Heart (US)", "en-US", "3"},
        {"kokoro-af_bella", "Bella (US)", "en-US", "2"},
        {"kokoro-af_nicole", "Nicole (US)", "en-US", "6"},
        {"kokoro-af_sarah", "Sarah (US)", "en-US", "9"},
        {"kokoro-am_michael", "Michael (US)", "en-US", "16"},
        {"kokoro-am_fenrir", "Fenrir (US)", "en-US", "14"},
        {"kokoro-am_puck", "Puck (US)", "en-US", "18"},
        {"kokoro-bf_emma", "Emma (UK)", "en-GB", "21"},
        {"kokoro-bf_isabella", "Isabella (UK)", "en-GB", "22"},
        {"kokoro-bf_alice", "Alice (UK)", "en-GB", "20"},
        {"kokoro-bm_george", "George (UK)", "en-GB", "26"},
        {"kokoro-bm_fable", "Fable (UK)", "en-GB", "25"},
        {"kokoro-bm_lewis", "Lewis (UK)", "en-GB", "27"},
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
    /** A voice was deleted: let go of the loaded model before speaking again. */
    private volatile boolean unload;

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

    /** Choose a voice (null: the first for the book's language). One not yet downloaded is
     *  fetched when first asked to speak. */
    void voice(String name) {
        chosen = name;
    }

    /** Fetch a voice now, in the background, so it is ready when chosen. */
    void download(String voice) {
        new Thread(() -> download(voice, true), "natural-voice-download").start();
    }

    /** Delete a downloaded voice (for Kokoro, all of its voices: they share one model). */
    void remove(String voice) {
        unload = true;
        delete(voiceDir(voice));
    }

    private static void delete(File file) {
        File[] children = file.listFiles();
        if (children != null) for (File child : children) delete(child);
        if (!file.delete() && file.exists()) Log.w(TAG, "could not delete " + file);
    }

    String current() {
        if (chosen != null) {
            for (String[] v : CATALOGUE) if (v[0].equals(chosen)) return chosen;
        }
        String lang = language.toLowerCase(Locale.ROOT).replace('_', '-');
        // Left to choose: a downloaded Piper voice in the book's language, else the first Piper
        // voice in it, else Alan. (Kokoro is only used when asked for: it is a big download.)
        for (String[] v : CATALOGUE) {
            if (!kokoro(v[0]) && lang.startsWith(v[2].toLowerCase(Locale.ROOT)) && downloaded(v[0])) return v[0];
        }
        for (String[] v : CATALOGUE) if (!kokoro(v[0]) && lang.startsWith(v[2].toLowerCase(Locale.ROOT))) return v[0];
        for (String[] v : CATALOGUE) if (!kokoro(v[0]) && downloaded(v[0])) return v[0];
        return "en_GB-alan-medium";
    }

    /**
     * All the voices, as "name, language tag, label, megabytes still to download, removable
     * (1 or 0), group, shared (1: the group downloads and deletes as one)", tab-separated.
     */
    String[] voices() {
        List<String> out = new ArrayList<>();
        for (String[] v : CATALOGUE) {
            boolean have = downloaded(v[0]);
            int size = have ? 0 : kokoro(v[0]) ? 360 : 60;
            String group = kokoro(v[0]) ? "Kokoro\t1" : "Piper\t0";
            out.add(v[0] + "\t" + v[2] + "\t" + v[1] + "\t" + size + "\t" + (have ? 1 : 0) + "\t" + group);
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
                if (unload) {
                    unload = false;
                    if (tts != null) tts.release();
                    tts = null;
                    loaded = null;
                }
                if (!downloaded(voice) && !download(voice, false)) {
                    Narration.heard(Narration.FAILED, job.id);
                    continue;
                }
                if (!model(voice).equals(loaded)) load(voice);
                if (job.generation != generation) continue;
                GeneratedAudio speech = tts.generate(job.text, speaker(voice), rate);
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

    private static boolean kokoro(String voice) {
        return voice.startsWith(KOKORO_PREFIX);
    }

    /**
     * Which model a voice needs loaded: each Piper voice is its own, while Kokoro's voices
     * share one, set up for American or British pronunciation.
     */
    private static String model(String voice) {
        if (!kokoro(voice)) return voice;
        return voice.startsWith(KOKORO_PREFIX + "b") ? "kokoro-gb" : "kokoro-us";
    }

    /** The speaker within the model. */
    private static int speaker(String voice) {
        for (String[] v : CATALOGUE) if (v[0].equals(voice) && v.length > 3) return Integer.parseInt(v[3]);
        return 0;
    }

    private void load(String voice) throws IOException {
        if (tts != null) tts.release();
        tts = null;
        loaded = null;
        File dir = voiceDir(voice);
        int cores = Runtime.getRuntime().availableProcessors();
        OfflineTtsModelConfig.Builder model = OfflineTtsModelConfig.builder().setDebug(false);
        if (kokoro(voice)) {
            String lexicon = model(voice).equals("kokoro-gb") ? "lexicon-gb-en.txt" : "lexicon-us-en.txt";
            model.setKokoro(OfflineTtsKokoroModelConfig.builder()
                            .setModel(new File(dir, "model.onnx").getPath())
                            .setVoices(new File(dir, "voices.bin").getPath())
                            .setTokens(new File(dir, "tokens.txt").getPath())
                            .setLexicon(new File(dir, lexicon).getPath())
                            .setDataDir(espeakData().getPath())
                            .build())
                    // Kokoro is the heavier model: give it more of the phone.
                    .setNumThreads(Math.max(2, Math.min(6, cores - 2)));
        } else {
            model.setVits(OfflineTtsVitsModelConfig.builder()
                            .setModel(new File(dir, "model.onnx").getPath())
                            .setTokens(new File(dir, "tokens.txt").getPath())
                            .setDataDir(espeakData().getPath())
                            .build())
                    .setNumThreads(Math.max(1, Math.min(4, cores / 2)));
        }
        tts = new OfflineTts(OfflineTtsConfig.builder().setModel(model.build()).build());
        loaded = model(voice);
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
        return new File(context.getFilesDir(), "voices/" + (kokoro(voice) ? KOKORO_DIR : voice));
    }

    /** The files a voice needs, as they are named once downloaded. */
    private static String[] files(String voice) {
        if (kokoro(voice)) {
            return new String[] {"tokens.txt", "lexicon-us-en.txt", "lexicon-gb-en.txt", "voices.bin", "model.onnx"};
        }
        return new String[] {"tokens.txt", "model.onnx"};
    }

    private boolean downloaded(String voice) {
        File dir = voiceDir(voice);
        for (String file : files(voice)) if (!new File(dir, file).isFile()) return false;
        return true;
    }

    /**
     * Fetch a voice (once, however many ask), reporting progress to the reader. Asked for
     * outright, it reports even a voice that is already there, so the list catches up.
     */
    private boolean download(String voice, boolean asked) {
        synchronized (downloadLock) {
            if (downloaded(voice)) {
                if (asked) Narration.progress(voice, 1f);
                return true;
            }
            File dir = voiceDir(voice);
            dir.mkdirs();
            try {
                // The small files first; the model, which is nearly all of it, shows progress.
                for (String file : files(voice)) {
                    File to = new File(dir, file);
                    if (to.isFile()) continue;
                    String url;
                    if (kokoro(voice)) {
                        url = KOKORO + file;
                    } else {
                        url = MODELS + voice + "/resolve/main/" + (file.equals("model.onnx") ? voice + ".onnx" : file);
                    }
                    fetch(url, to, file.equals("model.onnx") ? voice : null);
                }
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
