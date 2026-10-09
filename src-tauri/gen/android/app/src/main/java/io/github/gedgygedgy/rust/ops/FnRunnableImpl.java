package io.github.gedgygedgy.rust.ops;

import java.io.Closeable;

final class FnRunnableImpl implements FnRunnable {
    private final FnAdapter<FnRunnableImpl, Void, Void, Void> adapter;

    private FnRunnableImpl(FnAdapter<FnRunnableImpl, Void, Void, Void> adapter) {
        this.adapter = adapter;
    }

    @Override
    public void run() {
        this.adapter.call(this, null, null);
    }

    @Override
    public void close() {
        this.adapter.close();
    }
}
