// Copyright 2024 Xiaomi Corporation
//
// From sherpa-onnx v1.13.8 (Apache-2.0), cut down to what Rusty Reader uses, and loading the
// native library the Android way. The classes beside it are copied unchanged: the native code
// reads their fields by name.

package com.k2fsa.sherpa.onnx;

public class OfflineTts {
    static {
        System.loadLibrary("sherpa-onnx-jni");
    }

    private long ptr;

    public OfflineTts(OfflineTtsConfig config) {
        ptr = newFromFile(config);
        if (ptr == 0) {
            throw new IllegalArgumentException("Invalid OfflineTtsConfig: failed to create native OfflineTts");
        }
    }

    public int getSampleRate() {
        return getSampleRate(ptr);
    }

    /** Speech for the text, at `speed` (1.0 is the voice's own pace). */
    public GeneratedAudio generate(String text, int sid, float speed) {
        return generateImpl(ptr, text, sid, speed);
    }

    public void release() {
        if (ptr != 0) {
            delete(ptr);
            ptr = 0;
        }
    }

    private native void delete(long ptr);

    private native int getSampleRate(long ptr);

    private native GeneratedAudio generateImpl(long ptr, String text, int sid, float speed);

    private native long newFromFile(OfflineTtsConfig config);
}
