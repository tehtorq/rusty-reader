package com.rustyreader;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.os.Process;

/**
 * Starts Rusty Reader afresh, in a new process. The app's window system can only start once
 * per process, so when Android reuses a process the app already ran in (for instance one kept
 * alive by reading aloud), the app would hang on its launch screen. Instead it asks this
 * activity, which runs in a process of its own, to end the old process and launch it again.
 */
public class RestartActivity extends Activity {
    private static final String PID = "pid", AGAIN = "again";

    /** Called from Rust, on the activity that couldn't start. */
    static void restart(Activity from) {
        Intent again = new Intent(from.getIntent());
        from.startActivity(new Intent(from, RestartActivity.class)
                .putExtra(PID, Process.myPid())
                .putExtra(AGAIN, again)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
    }

    @Override
    @SuppressWarnings("deprecation") // The typed getParcelableExtra is Android 13+.
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        int pid = getIntent().getIntExtra(PID, -1);
        if (pid > 0) Process.killProcess(pid);
        Intent again = getIntent().getParcelableExtra(AGAIN);
        if (again == null) again = new Intent(this, ReaderActivity.class);
        again.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TASK);
        startActivity(again);
        finish();
        Runtime.getRuntime().exit(0);
    }
}
