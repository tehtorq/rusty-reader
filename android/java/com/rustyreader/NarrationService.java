package com.rustyreader;

import android.app.Notification;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.IBinder;

/**
 * Keeps the app running while it reads aloud with the screen off, showing the reading-aloud
 * notification. Its buttons come back here, and go on to the Rust narrator.
 */
public class NarrationService extends Service {
    /** Built by {@link Narration}, which knows what is being read. */
    static volatile Notification notification;

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent != null && intent.getAction() != null) {
            // A button on the notification.
            Narration.control(Integer.parseInt(intent.getAction()));
            return START_NOT_STICKY;
        }
        if (notification != null) {
            startForeground(Narration.NOTIFICATION, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK);
        }
        return START_NOT_STICKY;
    }

    /** Swiped away from recent apps: reading aloud ends with the app. */
    @Override
    public void onTaskRemoved(Intent rootIntent) {
        Narration.closed();
        stopSelf();
        super.onTaskRemoved(rootIntent);
    }

    @Override
    public void onDestroy() {
        stopForeground(STOP_FOREGROUND_REMOVE);
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }
}
