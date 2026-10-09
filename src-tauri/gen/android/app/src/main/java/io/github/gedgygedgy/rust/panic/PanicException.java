package io.github.gedgygedgy.rust.panic;

/**
 * Exception that represents a Rust panic. This exception can be used to carry
 * Rust panics across the JNI boundary.
 * <p>
 * Instances of this class cannot be obtained directly from Java. Instead, call
 * {@code jni_utils::exceptions::JPanicException::new())} to create a new
 * instance of this class.
 */
public final class PanicException extends RuntimeException {
    @SuppressWarnings("unused") // Native code uses this field.
    private long any;

    private PanicException(String message) {
        super(message);
    }
}
