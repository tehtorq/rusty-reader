#!/usr/bin/env bash
# Build an installable APK, without Gradle: cargo-ndk compiles the library, and the SDK's
# own aapt2, zipalign and apksigner package and sign it.
#
#   android/build.sh                  # phones (arm64)
#   ABIS="arm64-v8a x86_64" android/build.sh   # phones and the emulator
#   android/build.sh --install        # …and install on the connected device
set -euo pipefail
cd "$(dirname "$0")/.."

SDK="${ANDROID_HOME:-$HOME/Android/Sdk}"
export ANDROID_HOME="$SDK"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$(ls -d "$SDK"/ndk/* | sort -V | tail -1)}"
TOOLS="$(ls -d "$SDK"/build-tools/* | sort -V | tail -1)"
PLATFORM="$(ls -d "$SDK"/platforms/android-* | sort -V | tail -1)"
ABIS="${ABIS:-arm64-v8a}"
OUT=target/android
STRIP="$(ls "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin/llvm-strip)"
if [ -z "${JAVAC:-}" ]; then
    for candidate in "$(command -v javac || true)" "${JAVA_HOME:-}/bin/javac" "$HOME"/.local/share/jdk-*/bin/javac; do
        if [ -x "$candidate" ]; then JAVAC="$candidate"; break; fi
    done
fi
if [ -z "${JAVAC:-}" ]; then
    echo "No javac found: install a JDK (17 or newer), or set JAVAC=/path/to/javac" >&2
    exit 1
fi

rm -rf "$OUT/lib"
mkdir -p "$OUT/lib"
targets=()
for abi in $ABIS; do targets+=(-t "$abi"); done
cargo ndk "${targets[@]}" -P 30 -o "$OUT/lib" build --release --lib
for so in "$OUT"/lib/*/*.so; do "$STRIP" --strip-unneeded "$so"; done

# The Java activity that receives books, compiled to Android's dex format.
rm -rf "$OUT/classes" "$OUT/dex"
mkdir -p "$OUT/classes" "$OUT/dex"
"$JAVAC" --release 17 -nowarn -classpath "$PLATFORM/android.jar" -d "$OUT/classes" \
    $(find android/java -name '*.java')
"$TOOLS/d8" --release --min-api 30 --lib "$PLATFORM/android.jar" --output "$OUT/dex" \
    $(find "$OUT/classes" -name '*.class')

# Resources: the launcher icon.
rm -f "$OUT/res.zip"
"$TOOLS/aapt2" compile --dir android/res -o "$OUT/res.zip"
"$TOOLS/aapt2" link -o "$OUT/unsigned.apk" --manifest android/AndroidManifest.xml -I "$PLATFORM/android.jar" \
    "$OUT/res.zip"
(cd "$OUT" && zip -qr unsigned.apk lib && zip -qj unsigned.apk dex/classes.dex)
"$TOOLS/zipalign" -f -P 16 4 "$OUT/unsigned.apk" "$OUT/aligned.apk"

KEYSTORE="$HOME/.android/debug.keystore"
if [ ! -f "$KEYSTORE" ]; then
    mkdir -p "$(dirname "$KEYSTORE")"
    keytool -genkeypair -keystore "$KEYSTORE" -storepass android -keypass android -alias androiddebugkey \
        -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=Android Debug,O=Android,C=US"
fi
"$TOOLS/apksigner" sign --ks "$KEYSTORE" --ks-pass pass:android --key-pass pass:android \
    --out "$OUT/rusty-reader.apk" "$OUT/aligned.apk"
rm -f "$OUT/unsigned.apk" "$OUT/aligned.apk"
echo "Built $OUT/rusty-reader.apk ($(du -h "$OUT/rusty-reader.apk" | cut -f1))"

if [ "${1:-}" = "--install" ]; then
    "$SDK/platform-tools/adb" install -r "$OUT/rusty-reader.apk"
fi
