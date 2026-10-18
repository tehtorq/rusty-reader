package com.rustyreader;

import android.app.NativeActivity;
import android.content.ClipData;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.provider.OpenableColumns;
import android.util.Log;
import android.view.Display;
import android.view.WindowManager;

import java.io.BufferedInputStream;
import java.io.DataInputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.ConcurrentLinkedQueue;

/**
 * The stock NativeActivity, plus the one thing Rust can't do alone: receive books from
 * Android's document picker, "Open with" and "Share". Books are copied into the app's
 * Books folder, and their paths left for the Rust side to collect with takePickedBooks().
 */
public class ReaderActivity extends NativeActivity {
    private static final String TAG = "RustyReader";
    private static final int PICK_BOOK = 1;

    /** Reading aloud; Rust calls the methods below from its narrator thread. */
    private final Narration narration = new Narration(this);

    /** Books that arrived together, published once all of them have been copied. */
    private final ConcurrentLinkedQueue<String[]> arrived = new ConcurrentLinkedQueue<>();

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        // Draw behind the camera cut-out rather than letterboxing it in black; the Rust side
        // keeps text clear of it using the window insets.
        WindowManager.LayoutParams attrs = getWindow().getAttributes();
        attrs.layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS;
        // Ask for the display's fastest refresh rate: left to itself, the system may run a
        // NativeActivity at 60 Hz, or keep changing its mind, which shows as judder in motion.
        Display display = getDisplay();
        if (display != null) {
            Display.Mode current = display.getMode();
            Display.Mode fastest = current;
            for (Display.Mode mode : display.getSupportedModes()) {
                if (mode.getPhysicalWidth() == current.getPhysicalWidth()
                        && mode.getPhysicalHeight() == current.getPhysicalHeight()
                        && mode.getRefreshRate() > fastest.getRefreshRate()) {
                    fastest = mode;
                }
            }
            attrs.preferredDisplayModeId = fastest.getModeId();
            attrs.preferredRefreshRate = fastest.getRefreshRate();
        }
        getWindow().setAttributes(attrs);
        receive(getIntent());
    }

    @Override
    protected void onDestroy() {
        // Closing the app closes the book, voice and all.
        if (isFinishing()) Narration.closed();
        super.onDestroy();
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        receive(intent);
    }

    /** Called from Rust: the display's current refresh rate, for the frame meter. */
    public float refreshRate() {
        Display display = getDisplay();
        return display == null ? 0f : display.getRefreshRate();
    }

    // Called from Rust's narrator: see Narration.
    public void speak(long id, String text, boolean paragraphEnd) { narration.speak(id, text, paragraphEnd); }
    public void silence() { narration.silence(); }
    public void speechRate(float rate) { narration.rate(rate); }
    public void speechLanguage(String tag) { narration.language(tag); }
    public void speechVoice(String name) { narration.voice(name); }
    public String[] speechVoices() { return narration.voices(); }
    public void speechEngine(String name) { narration.engine(name); }
    public String[] speechEngines() { return narration.engines(); }
    public String speechCurrentVoice() { return narration.currentVoice(); }
    public void nowPlaying(int state, String title, String author) { narration.nowPlaying(state, title, author); }

    /** Called from Rust: a short buzz, as for any long press. */
    public void buzz() {
        runOnUiThread(() -> getWindow().getDecorView()
                .performHapticFeedback(android.view.HapticFeedbackConstants.LONG_PRESS));
    }

    /** Called from Rust: show the system document picker. */
    public void pickBook() {
        runOnUiThread(() -> {
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            intent.setType("*/*");
            // Some storage providers don't know the EPUB type, so offer generic files too.
            intent.putExtra(Intent.EXTRA_MIME_TYPES,
                    new String[] {"application/epub+zip", "application/octet-stream"});
            intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
            startActivityForResult(intent, PICK_BOOK);
        });
    }

    /** Called from Rust: the next batch of received books, or null. */
    public String[] takePickedBooks() {
        return arrived.poll();
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request != PICK_BOOK || result != RESULT_OK || data == null) return;
        List<Uri> uris = new ArrayList<>();
        ClipData clip = data.getClipData();
        if (clip != null) {
            for (int i = 0; i < clip.getItemCount(); i++) uris.add(clip.getItemAt(i).getUri());
        } else if (data.getData() != null) {
            uris.add(data.getData());
        }
        receive(uris);
    }

    // The typed getParcelable…Extra overloads are Android 13+; these work back to our minimum.
    @SuppressWarnings("deprecation")
    private void receive(Intent intent) {
        if (intent == null || intent.getAction() == null) return;
        List<Uri> uris = new ArrayList<>();
        switch (intent.getAction()) {
            case Intent.ACTION_VIEW:
                if (intent.getData() != null) uris.add(intent.getData());
                break;
            case Intent.ACTION_SEND:
                Uri uri = intent.getParcelableExtra(Intent.EXTRA_STREAM);
                if (uri != null) uris.add(uri);
                break;
            case Intent.ACTION_SEND_MULTIPLE:
                ArrayList<Uri> shared = intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM);
                if (shared != null) uris.addAll(shared);
                break;
        }
        receive(uris);
    }

    /** Copy books off the UI thread: a large batch would otherwise freeze the app. */
    private void receive(List<Uri> uris) {
        if (uris.isEmpty()) return;
        new Thread(() -> {
            List<String> paths = new ArrayList<>();
            for (Uri uri : uris) {
                String path = copy(uri);
                if (path != null) paths.add(path);
            }
            if (!paths.isEmpty()) arrived.add(paths.toArray(new String[0]));
        }, "receive-books").start();
    }

    private String copy(Uri uri) {
        if (uri == null) return null;
        if ("file".equals(uri.getScheme())) return uri.getPath();
        File dir = new File(getExternalFilesDir(null), "Books");
        dir.mkdirs();
        File incoming = null;
        try {
            // Copied under a temporary name, so a half-written book is never mistaken for one.
            incoming = File.createTempFile(".incoming-", ".part", dir);
            try (InputStream in = getContentResolver().openInputStream(uri);
                    OutputStream os = new FileOutputStream(incoming)) {
                if (in == null) throw new IOException("No content");
                byte[] buffer = new byte[64 * 1024];
                int n;
                while ((n = in.read(buffer)) > 0) os.write(buffer, 0, n);
            }
            return place(incoming, dir, displayName(uri)).getAbsolutePath();
        } catch (Exception e) {
            Log.e(TAG, "Could not copy book " + uri, e);
            if (incoming != null) incoming.delete();
            return null;
        }
    }

    /**
     * Give a freshly copied book its name. The same file imported again reuses the copy
     * already there; a different book that happens to share the name becomes "Name (2)".
     */
    private static File place(File incoming, File dir, String name) throws IOException {
        String stem = name.substring(0, name.length() - ".epub".length());
        for (int n = 1; ; n++) {
            File candidate = new File(dir, n == 1 ? name : stem + " (" + n + ").epub");
            if (!candidate.exists()) {
                if (!incoming.renameTo(candidate)) throw new IOException("Could not rename to " + candidate);
                return candidate;
            }
            if (sameContents(candidate, incoming)) {
                incoming.delete();
                return candidate;
            }
        }
    }

    private static boolean sameContents(File a, File b) throws IOException {
        if (a.length() != b.length()) return false;
        try (DataInputStream x = new DataInputStream(new BufferedInputStream(new FileInputStream(a)));
                DataInputStream y = new DataInputStream(new BufferedInputStream(new FileInputStream(b)))) {
            byte[] p = new byte[64 * 1024];
            byte[] q = new byte[64 * 1024];
            for (long left = a.length(); left > 0; ) {
                int chunk = (int) Math.min(p.length, left);
                x.readFully(p, 0, chunk);
                y.readFully(q, 0, chunk);
                // (Arrays.equals with ranges is Android 13+.)
                for (int i = 0; i < chunk; i++) if (p[i] != q[i]) return false;
                left -= chunk;
            }
            return true;
        }
    }

    private String displayName(Uri uri) {
        String name = null;
        try (Cursor c = getContentResolver()
                .query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (c != null && c.moveToFirst()) name = c.getString(0);
        } catch (Exception e) {
            Log.w(TAG, "No display name for " + uri, e);
        }
        if (name == null || name.isEmpty()) name = "Book";
        name = name.replaceAll("[/\\\\]", "_");
        if (!name.toLowerCase().endsWith(".epub")) name += ".epub";
        return name;
    }
}
