package io.github.gedgygedgy.rust.ops;

import java.io.Closeable;

final class FnFunctionImpl<T, R> implements FnFunction<T, R> {
    private final FnAdapter<FnFunctionImpl<T, R>, T, Void, R> adapter;

    private FnFunctionImpl(FnAdapter<FnFunctionImpl<T, R>, T, Void, R> adapter) {
        this.adapter = adapter;
    }

    @Override
    public R apply(T t) {
        return this.adapter.call(this, t, null);
    }

    @Override
    public void close() {
        this.adapter.close();
    }
}
