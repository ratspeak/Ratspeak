package io.github.gedgygedgy.rust.stream;

/**
 * Represents the result of polling an async stream.
 * <p>
 * See {@link Stream#pollNext} for a description of how to use this interface.
 */
public interface StreamPoll<T> {
    /**
     * Gets the stream item. This can be anything, including {@code null}.
     *
     * @return The stream item.
     */
    T get();
}
