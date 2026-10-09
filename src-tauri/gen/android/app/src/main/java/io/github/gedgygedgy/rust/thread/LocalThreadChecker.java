package io.github.gedgygedgy.rust.thread;

public class LocalThreadChecker {
    private final Thread origin;

    public LocalThreadChecker(boolean local) {
        this.origin = local ? Thread.currentThread() : null;
    }

    public void check() {
        if (this.origin != null && this.origin != Thread.currentThread()) {
            throw new LocalThreadException();
        }
    }
}
