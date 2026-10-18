package com.rustyreader;

import android.app.Activity;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.graphics.Bitmap;
import android.graphics.drawable.Drawable;
import android.media.AudioAttributes;
import android.media.AudioFocusRequest;
import android.media.AudioManager;
import android.media.MediaMetadata;
import android.media.session.MediaSession;
import android.media.session.PlaybackState;
import android.os.PowerManager;
import android.speech.tts.TextToSpeech;
import android.speech.tts.UtteranceProgressListener;
import android.speech.tts.Voice;
import android.util.Log;

import java.util.ArrayList;
import java.util.Comparator;
import java.util.HashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;

/**
 * Reading aloud, Android's half: the system text-to-speech engine, and everything that lets
 * it carry on with the screen off: a media session (lock screen, headphone buttons), audio
 * focus (pausing for calls), and a foreground service with its notification.
 *
 * The Rust narrator decides what to say and when; it calls in here from its own thread and
 * hears back through the native methods below.
 */
final class Narration {
    private static final String TAG = "RustyReader";
    private static final String CHANNEL = "reading-aloud";
    static final int NOTIFICATION = 1;

    // Kinds for heard(), and actions for control(); the Rust side matches these numbers.
    static final int STARTED = 0, DONE = 1, FAILED = 2;
    static final int PLAY = 0, PAUSE = 1, TOGGLE = 2, STOP = 3, NEXT = 4, PREVIOUS = 5;

    /** An utterance began, finished or failed. Registered from Rust at startup. */
    static native void heard(int kind, long id);
    /** A request from the lock screen, headphones or the system. Registered from Rust. */
    static native void control(int action);
    /** How far a voice download has got (0 to 1; -1 if it failed). Registered from Rust. */
    static native void progress(String voice, float fraction);
    /** Reading aloud went wrong: what happened, to show. Registered from Rust. */
    static native void failure(String message);

    // The narrator's states, as Rust numbers them.
    private static final int IDLE = 0, PLAYING = 1, PAUSED = 2;

    private final Activity activity;
    private final AudioAttributes audio = new AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_MEDIA)
            .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
            .build();

    private static final String GOOGLE = "com.google.android.tts";
    /** The engine id for the app's own natural voices, in the list beside the system's. */
    private static final String NATURAL = "rustyreader.natural";

    /** The app's own voices; started when first needed. */
    private NaturalVoices natural;

    private TextToSpeech tts;
    private CountDownLatch ready = new CountDownLatch(1);
    private boolean usable;
    /** The engine asked for (a package name), or null for the app's choice. */
    private String wantedEngine;
    private String runningEngine;
    private Locale language = Locale.getDefault();
    private String voice;
    private float rate = 1f;

    private MediaSession session;
    private AudioFocusRequest focus;
    private boolean pausedForFocus;
    private boolean noisyRegistered;
    /** Keeps the processor awake between sentences while reading with the screen off. */
    private PowerManager.WakeLock wake;
    private int state = IDLE;
    private String title = "", author = "";

    private final BroadcastReceiver noisy = new BroadcastReceiver() {
        @Override
        public void onReceive(Context context, Intent intent) {
            // Headphones unplugged: don't carry on out loud.
            control(PAUSE);
        }
    };

    /** The one in use, for the service to reach when the app is swiped away. */
    private static volatile Narration current;

    Narration(Activity activity) {
        this.activity = activity;
        current = this;
    }

    /**
     * The app was closed (swiped away, or its activity finished): fall silent at once, and tell
     * the narrator, rather than read on with nothing on screen.
     */
    static void closed() {
        control(STOP);
        Narration narration = current;
        if (narration != null) {
            narration.silence();
            narration.activity.runOnUiThread(narration::stopAll);
        }
    }

    // ---- The voice --------------------------------------------------------------------

    /**
     * The engine, started on first use and restarted when another is chosen; blocks (off the UI
     * thread) until it is ready. Left to choose, the app prefers Google's engine where it is
     * installed: phone makers' own engines tend to sound robotic, and some ignore the pace and
     * voice asked for.
     */
    private synchronized TextToSpeech engine() {
        if (tts == null || !java.util.Objects.equals(runningEngine, chosenEngine())) {
            if (tts != null) tts.shutdown();
            String engine = chosenEngine();
            CountDownLatch latch = new CountDownLatch(1);
            ready = latch;
            usable = false;
            runningEngine = engine;
            TextToSpeech[] made = new TextToSpeech[1];
            TextToSpeech.OnInitListener onInit = status -> {
                synchronized (Narration.this) {
                    if (made[0] == tts) {
                        usable = status == TextToSpeech.SUCCESS;
                        if (usable) {
                            setUp(tts);
                        } else {
                            Log.w(TAG, "text-to-speech failed to start: " + status);
                        }
                    }
                }
                latch.countDown();
            };
            Context context = activity.getApplicationContext();
            made[0] = engine == null ? new TextToSpeech(context, onInit) : new TextToSpeech(context, onInit, engine);
            tts = made[0];
        }
        return tts;
    }

    private void setUp(TextToSpeech engine) {
        engine.setAudioAttributes(audio);
        engine.setOnUtteranceProgressListener(new UtteranceProgressListener() {
            @Override public void onStart(String id) { report(STARTED, id); }
            @Override public void onDone(String id) { report(DONE, id); }
            @Override @Deprecated public void onError(String id) { report(FAILED, id); }
            @Override public void onError(String id, int code) { report(FAILED, id); }
        });
        engine.setSpeechRate(rate);
        applyVoice();
    }

    private static void report(int kind, String id) {
        long n = utterance(id);
        if (n >= 0) heard(kind, n);
    }

    /** The engine to use: the one asked for if it is installed, else Google's, else the default. */
    private String chosenEngine() {
        List<String> installed = new ArrayList<>();
        for (android.content.pm.ResolveInfo info : activity.getPackageManager().queryIntentServices(
                new Intent(TextToSpeech.Engine.INTENT_ACTION_TTS_SERVICE), 0)) {
            installed.add(info.serviceInfo.packageName);
        }
        if (wantedEngine != null && installed.contains(wantedEngine)) return wantedEngine;
        return installed.contains(GOOGLE) ? GOOGLE : null;
    }

    private boolean waitReady() {
        CountDownLatch latch;
        synchronized (this) {
            engine();
            latch = ready;
        }
        try {
            latch.await(8, TimeUnit.SECONDS);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
        return usable;
    }

    /** Whether to speak with the app's own voices: unless another engine was chosen. */
    private boolean useNatural() {
        return wantedEngine == null || wantedEngine.equals(NATURAL);
    }

    private synchronized NaturalVoices natural() {
        if (natural == null) {
            natural = new NaturalVoices(activity, audio);
            natural.rate(rate);
            natural.language(language.toLanguageTag());
        }
        return natural;
    }

    void speak(long id, String text, boolean paragraphEnd) {
        if (useNatural()) {
            natural().speak(id, text, paragraphEnd);
        } else if (!waitReady()
                || tts.speak(text, TextToSpeech.QUEUE_ADD, null, Long.toString(id)) != TextToSpeech.SUCCESS) {
            heard(FAILED, id);
        } else if (paragraphEnd) {
            // A breath between paragraphs; its id isn't a number, so it reports nothing.
            tts.playSilentUtterance((long) (450 / rate), TextToSpeech.QUEUE_ADD, "pause-" + id);
        }
    }

    /** An utterance's id as the narrator numbered it, or -1 for the app's own pauses. */
    private static long utterance(String id) {
        try {
            return Long.parseLong(id);
        } catch (NumberFormatException e) {
            return -1;
        }
    }

    void silence() {
        if (natural != null) natural.stop();
        if (tts != null && usable) tts.stop();
    }

    void rate(float rate) {
        this.rate = rate;
        if (useNatural()) natural().rate(rate);
        else if (waitReady()) tts.setSpeechRate(rate);
    }

    void language(String tag) {
        language = Locale.forLanguageTag(tag);
        if (useNatural()) natural().language(tag);
        else if (waitReady()) applyVoice();
    }

    void voice(String name) {
        voice = name;
        if (useNatural()) natural().voice(name);
        else if (waitReady()) applyVoice();
    }

    /** Switch engine (an engine's package, NATURAL, or null for the app's choice). */
    void engine(String name) {
        silence();
        wantedEngine = name;
        if (useNatural()) {
            natural().rate(rate);
            natural().language(language.toLanguageTag());
        } else {
            waitReady();
        }
    }

    private void applyVoice() {
        if (voice != null && tts.getVoices() != null) {
            for (Voice v : tts.getVoices()) {
                if (v.getName().equals(voice)) {
                    tts.setVoice(v);
                    return;
                }
            }
        }
        tts.setLanguage(language);
    }

    /** The name of the voice that will speak, or null if the engine won't say. */
    String currentVoice() {
        if (useNatural()) return natural().current();
        if (!waitReady()) return null;
        Voice v = tts.getVoice();
        return v == null ? null : v.getName();
    }

    /** The app's own voices and the installed engines, as "id\tname", the one in use first. */
    String[] engines() {
        List<String[]> engines = new ArrayList<>();
        engines.add(new String[] {NATURAL, "Natural voices"});
        android.content.pm.PackageManager pm = activity.getPackageManager();
        for (android.content.pm.ResolveInfo info : pm.queryIntentServices(
                new Intent(TextToSpeech.Engine.INTENT_ACTION_TTS_SERVICE), 0)) {
            engines.add(new String[] {info.serviceInfo.packageName, info.loadLabel(pm).toString()});
        }
        String system = chosenEngine();
        if (system == null) {
            system = android.provider.Settings.Secure.getString(activity.getContentResolver(), "tts_default_synth");
        }
        String current = useNatural() ? NATURAL : system;
        engines.sort(Comparator.comparing((String[] e) -> !e[0].equals(current)));
        String[] out = new String[engines.size()];
        for (int i = 0; i < out.length; i++) out[i] = engines.get(i)[0] + "\t" + engines.get(i)[1];
        return out;
    }

    /**
     * Voices of the engine in use, as "name\tlanguage tag\tlabel": those that work offline first,
     * then online ones (often the most natural), best first within each.
     */
    String[] voices() {
        if (useNatural()) return natural().voices();
        if (!waitReady() || tts.getVoices() == null) return new String[0];
        List<Voice> usable = new ArrayList<>();
        for (Voice v : tts.getVoices()) {
            boolean missing = v.getFeatures() != null
                    && v.getFeatures().contains(TextToSpeech.Engine.KEY_FEATURE_NOT_INSTALLED);
            if (!missing || v.isNetworkConnectionRequired()) usable.add(v);
        }
        usable.sort(Comparator.comparing((Voice v) -> v.getLocale().toLanguageTag())
                .thenComparing(Voice::isNetworkConnectionRequired)
                .thenComparing(v -> -v.getQuality())
                .thenComparing(Voice::getName));
        Map<String, Integer> counts = new HashMap<>();
        String[] out = new String[usable.size()];
        for (int i = 0; i < out.length; i++) {
            Voice v = usable.get(i);
            String place = v.getLocale().getDisplayCountry();
            String region = place.isEmpty() ? v.getLocale().getDisplayLanguage() : place;
            int n = counts.merge(region, 1, Integer::sum);
            String online = v.isNetworkConnectionRequired() ? " (online)" : "";
            out[i] = v.getName() + "\t" + v.getLocale().toLanguageTag() + "\t" + region + " · voice " + n + online;
        }
        return out;
    }

    // ---- Playing in the background ----------------------------------------------------

    /** The narrator's state changed: keep the session, focus and notification in step. */
    void nowPlaying(int newState, String newTitle, String newAuthor) {
        activity.runOnUiThread(() -> {
            int was = state;
            state = newState;
            title = newTitle == null ? "" : newTitle;
            author = newAuthor == null ? "" : newAuthor;
            if (state == IDLE) {
                stopAll();
                return;
            }
            if (state == PLAYING && !requestFocus()) {
                control(PAUSE);
            }
            if (state == PLAYING && !noisyRegistered) {
                // A protected system broadcast, so no exported/not-exported flag is needed (the
                // flag itself only exists from Android 13).
                activity.registerReceiver(noisy, new IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY));
                noisyRegistered = true;
            } else if (state != PLAYING && noisyRegistered) {
                activity.unregisterReceiver(noisy);
                noisyRegistered = false;
            }
            if (wake == null) {
                wake = activity.getSystemService(PowerManager.class)
                        .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "RustyReader:reading-aloud");
                wake.setReferenceCounted(false);
            }
            if (state == PLAYING) wake.acquire();
            else wake.release();
            updateSession();
            Notification notification = buildNotification();
            NarrationService.notification = notification;
            if (was == IDLE) {
                // While reading (or paused) the service keeps the app alive with the screen off.
                activity.startForegroundService(new Intent(activity, NarrationService.class));
            } else {
                activity.getSystemService(NotificationManager.class).notify(NOTIFICATION, notification);
            }
        });
    }

    private void stopAll() {
        if (wake != null) wake.release();
        if (noisyRegistered) {
            activity.unregisterReceiver(noisy);
            noisyRegistered = false;
        }
        if (focus != null) {
            activity.getSystemService(AudioManager.class).abandonAudioFocusRequest(focus);
            focus = null;
        }
        if (session != null) {
            session.setActive(false);
            session.release();
            session = null;
        }
        activity.stopService(new Intent(activity, NarrationService.class));
    }

    private boolean requestFocus() {
        if (focus == null) {
            focus = new AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                    .setAudioAttributes(audio)
                    // Speech under someone else's sound is no use: pause rather than duck.
                    .setWillPauseWhenDucked(true)
                    .setOnAudioFocusChangeListener(change -> {
                        switch (change) {
                            case AudioManager.AUDIOFOCUS_LOSS:
                                pausedForFocus = false;
                                control(PAUSE);
                                break;
                            case AudioManager.AUDIOFOCUS_LOSS_TRANSIENT:
                            case AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK:
                                // A call or a navigation prompt: carry on once it is over.
                                if (state == PLAYING) {
                                    pausedForFocus = true;
                                    control(PAUSE);
                                }
                                break;
                            case AudioManager.AUDIOFOCUS_GAIN:
                                if (pausedForFocus) control(PLAY);
                                pausedForFocus = false;
                                break;
                        }
                    })
                    .build();
        }
        int result = activity.getSystemService(AudioManager.class).requestAudioFocus(focus);
        return result != AudioManager.AUDIOFOCUS_REQUEST_FAILED;
    }

    private void updateSession() {
        if (session == null) {
            session = new MediaSession(activity, "RustyReader");
            session.setCallback(new MediaSession.Callback() {
                @Override public void onPlay() { control(PLAY); }
                @Override public void onPause() { control(PAUSE); }
                @Override public void onStop() { control(STOP); }
                @Override public void onSkipToNext() { control(NEXT); }
                @Override public void onSkipToPrevious() { control(PREVIOUS); }
            });
            session.setSessionActivity(openApp());
        }
        session.setMetadata(new MediaMetadata.Builder()
                .putString(MediaMetadata.METADATA_KEY_TITLE, title)
                .putString(MediaMetadata.METADATA_KEY_ARTIST, author)
                .putBitmap(MediaMetadata.METADATA_KEY_ART, artwork())
                .build());
        session.setPlaybackState(new PlaybackState.Builder()
                .setActions(PlaybackState.ACTION_PLAY | PlaybackState.ACTION_PAUSE
                        | PlaybackState.ACTION_PLAY_PAUSE | PlaybackState.ACTION_STOP
                        | PlaybackState.ACTION_SKIP_TO_NEXT | PlaybackState.ACTION_SKIP_TO_PREVIOUS)
                .setState(state == PLAYING ? PlaybackState.STATE_PLAYING : PlaybackState.STATE_PAUSED,
                        PlaybackState.PLAYBACK_POSITION_UNKNOWN, 1f)
                .build());
        session.setActive(true);
    }

    private Bitmap art;

    /** The app icon, for the lock screen. */
    private Bitmap artwork() {
        if (art == null) {
            Drawable icon = activity.getApplicationInfo().loadIcon(activity.getPackageManager());
            art = Bitmap.createBitmap(256, 256, Bitmap.Config.ARGB_8888);
            icon.setBounds(0, 0, 256, 256);
            icon.draw(new android.graphics.Canvas(art));
        }
        return art;
    }

    private PendingIntent openApp() {
        Intent intent = new Intent(activity, ReaderActivity.class).setFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP);
        return PendingIntent.getActivity(activity, 0, intent, PendingIntent.FLAG_IMMUTABLE);
    }

    private Notification.Action action(int icon, String label, int what) {
        Intent intent = new Intent(activity, NarrationService.class).setAction(Integer.toString(what));
        PendingIntent pending = PendingIntent.getService(activity, what, intent, PendingIntent.FLAG_IMMUTABLE);
        return new Notification.Action.Builder(android.graphics.drawable.Icon.createWithResource(activity, icon),
                label, pending).build();
    }

    private Notification buildNotification() {
        NotificationManager manager = activity.getSystemService(NotificationManager.class);
        if (manager.getNotificationChannel(CHANNEL) == null) {
            NotificationChannel channel = new NotificationChannel(CHANNEL, "Reading aloud",
                    NotificationManager.IMPORTANCE_LOW);
            channel.setShowBadge(false);
            manager.createNotificationChannel(channel);
        }
        boolean playing = state == PLAYING;
        return new Notification.Builder(activity, CHANNEL)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle(title)
                .setContentText(author)
                .setLargeIcon(artwork())
                .setContentIntent(openApp())
                .setDeleteIntent(PendingIntent.getService(activity, STOP,
                        new Intent(activity, NarrationService.class).setAction(Integer.toString(STOP)),
                        PendingIntent.FLAG_IMMUTABLE))
                .setVisibility(Notification.VISIBILITY_PUBLIC)
                .setOngoing(playing)
                .setShowWhen(false)
                .addAction(action(R.drawable.ic_previous, "Previous sentence", PREVIOUS))
                .addAction(playing ? action(R.drawable.ic_pause, "Pause", PAUSE)
                        : action(R.drawable.ic_play, "Play", PLAY))
                .addAction(action(R.drawable.ic_next, "Next sentence", NEXT))
                .setStyle(new Notification.MediaStyle()
                        .setMediaSession(session.getSessionToken())
                        .setShowActionsInCompactView(0, 1, 2))
                .build();
    }
}
