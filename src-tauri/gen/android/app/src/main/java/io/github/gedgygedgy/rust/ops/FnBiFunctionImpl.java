package io.github.gedgygedgy.rust.ops;

import java.io.Closeable;

final class FnBiFunctionImpl<T, U, R> implements FnBiFunction<T, U, R> {
    private final FnAdapter<FnBiFunctionImpl<T, U, R>, T, U, R> adapter;

    private FnBiFunctionImpl(FnAdapter<FnBiFunctionImpl<T, U, R>, T, U, R> adapter) {
        this.adapter = adapter;
    }

    @Override
    public R apply(T t, U u) {
        return this.adapter.call(this, t, u);
    }

    @Override
    public void close() {
        this.adapter.close();
    }
}
