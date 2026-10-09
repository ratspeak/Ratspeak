package io.github.gedgygedgy.rust.thread;

/**
 * Thrown when a non-{@code Send} Rust object is accessed from outside its
 * origin thread.
 */
public class LocalThreadException extends IllegalStateException {
    /**
     * Creates a new exception.
     */
    public LocalThreadException() {}
}
