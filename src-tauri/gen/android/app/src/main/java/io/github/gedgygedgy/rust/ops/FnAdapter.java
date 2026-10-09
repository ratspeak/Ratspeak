package io.github.gedgygedgy.rust.ops;

import io.github.gedgygedgy.rust.thread.LocalThreadChecker;

import java.io.Closeable;

final class FnAdapter<O, T, U, R> implements Closeable {
    private final LocalThreadChecker threadChecker;
    private long data;

    private FnAdapter(boolean local) {
        this.threadChecker = new LocalThreadChecker(local);
    }

    public R call(O self, T arg1, U arg2) {
        this.threadChecker.check();
        return this.callInternal(self, arg1, arg2);
    }

    private native R callInternal(O self, T arg1, U arg2);

    @Override
    public void close() {
        this.threadChecker.check();
        this.closeInternal();
    }

    private native void closeInternal();
}
