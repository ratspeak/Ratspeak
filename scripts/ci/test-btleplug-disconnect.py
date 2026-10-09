#!/usr/bin/env python3
"""Exercise the shipped Java callback and future queue with a permission-denying GATT.

The small Android doubles run only on the host JVM; they are never packaged.
No Bluetooth adapter, native JNI library, or device is used.
"""
import argparse
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
STUBS = {
    "android/bluetooth/BluetoothAdapter.java": """package android.bluetooth;
public class BluetoothAdapter {
    public static BluetoothAdapter getDefaultAdapter() { return new BluetoothAdapter(); }
    public BluetoothDevice getRemoteDevice(String address) { return null; }
}
""",
    "android/bluetooth/BluetoothGattCallback.java": """package android.bluetooth;
public class BluetoothGattCallback {
    public void onConnectionStateChange(BluetoothGatt gatt, int status, int state) {}
}
""",
    "android/bluetooth/BluetoothGatt.java": """package android.bluetooth;
public class BluetoothGatt {
    public boolean denyClose;
    public int closeCalls;
    public boolean discoverServices() { return true; }
    public void close() {
        closeCalls++;
        if (denyClose) throw new SecurityException("permission revoked");
    }
}
""",
    "com/nonpolynomial/btleplug/android/impl/DisconnectPermissionTest.java": """package com.nonpolynomial.btleplug.android.impl;
import android.bluetooth.BluetoothGatt;
import android.bluetooth.BluetoothGattCallback;
import java.lang.reflect.Field;
import java.util.Queue;
import java.util.concurrent.atomic.AtomicBoolean;
import io.github.gedgygedgy.rust.future.FutureException;
import io.github.gedgygedgy.rust.future.SimpleFuture;
import io.github.gedgygedgy.rust.task.PollResult;

public final class DisconnectPermissionTest {
    private static Field field(Class<?> type, String name) throws Exception {
        Field field = type.getDeclaredField(name);
        field.setAccessible(true);
        return field;
    }
    private static Object get(Object target, String name) throws Exception {
        return field(target.getClass(), name).get(target);
    }
    @SuppressWarnings("unchecked")
    private static void check(boolean denied, boolean alreadyRetired) throws Exception {
        Peripheral peripheral = new Peripheral(null, "AA:BB:CC:DD:EE:01");
        BluetoothGatt gatt = new BluetoothGatt();
        gatt.denyClose = denied;
        field(Peripheral.class, "gatt").set(peripheral, gatt);
        field(Peripheral.class, "connected").set(peripheral, true);
        Object future = peripheral.discoverServices();
        BluetoothGattCallback callback = (BluetoothGattCallback) get(peripheral, "commandCallback");
        AtomicBoolean queueAdvanced = new AtomicBoolean();
        ((Queue<Runnable>) get(peripheral, "commandQueue")).add(() -> queueAdvanced.set(true));
        if (alreadyRetired) field(Peripheral.class, "gatt").set(peripheral, null);
        callback.onConnectionStateChange(gatt, 0, 0);
        if (get(peripheral, "gatt") != null) throw new AssertionError("stale GATT reference retained");
        if (!queueAdvanced.get()) throw new AssertionError("command queue stalled");
        if (gatt.closeCalls != (alreadyRetired ? 0 : 1)) throw new AssertionError("duplicate close");
        PollResult<?> result = (PollResult<?>) field(SimpleFuture.class, "result").get(future);
        if (result == null) throw new AssertionError("pending future was not completed");
        try {
            result.get();
            throw new AssertionError("disconnect falsely completed as success");
        } catch (FutureException error) {
            Class<?> expected = denied && !alreadyRetired ? PermissionDeniedException.class : NotConnectedException.class;
            if (!expected.isInstance(error.getCause())) throw new AssertionError("wrong disconnect failure", error);
            if (expected == PermissionDeniedException.class && !(error.getCause().getCause() instanceof SecurityException))
                throw new AssertionError("lost permission failure cause");
        }
    }
    public static void main(String[] args) throws Exception {
        check(false, false);
        check(true, false);
        check(true, true);
        System.out.println("btleplug disconnect: normal, revoked permission, and retired GATT pass; future and queue complete");
    }
}
""",
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", type=Path, default=ROOT,
                        help="app source root; permits an explicit unpatched negative control")
    parser.add_argument("--android-jar", type=Path, required=True)
    args = parser.parse_args()
    java_home = Path(os.environ["JAVA_HOME"])
    java = args.source_root / "src-tauri/gen/android/app/src/main/java"
    sources = sorted(p for namespace in ("com/nonpolynomial/btleplug", "io/github/gedgygedgy/rust")
                     for p in (java / namespace).rglob("*.java"))
    assert len(sources) == 28 and args.android_jar.is_file()
    with tempfile.TemporaryDirectory(prefix="btleplug-disconnect-") as directory:
        root = Path(directory)
        production = root / "production"
        doubles = root / "doubles"
        production.mkdir()
        doubles.mkdir()
        javac = [str(java_home / "bin/javac"), "-source", "8", "-target", "8", "-Xlint:-options"]
        subprocess.run(javac + ["-cp", str(args.android_jar), "-d", str(production)] + list(map(str, sources)), check=True)
        files = []
        for relative, source in STUBS.items():
            path = root / "stubs" / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(source)
            files.append(str(path))
        subprocess.run(javac + ["-cp", os.pathsep.join(map(str, [production, args.android_jar])), "-d", str(doubles)] + files, check=True)
        subprocess.run([str(java_home / "bin/java"), "-ea", "-cp", os.pathsep.join(map(str, [doubles, production, args.android_jar])),
                        "com.nonpolynomial.btleplug.android.impl.DisconnectPermissionTest"], check=True, timeout=30)


if __name__ == "__main__":
    main()
