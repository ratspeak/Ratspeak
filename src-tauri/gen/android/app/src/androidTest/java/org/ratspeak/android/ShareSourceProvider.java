package org.ratspeak.android;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;
import java.io.File;
import java.io.FileNotFoundException;

/** Only one synthetic, read-only fixture; no caller-controlled file paths. */
public class ShareSourceProvider extends ContentProvider {
    static final Uri URI = Uri.parse("content://org.ratspeak.android.test.photos/photo");
    @Override public boolean onCreate() { return true; }
    private File file(Uri uri) {
        if (!URI.equals(uri)) throw new SecurityException("Synthetic photo only");
        return new File(getContext().getCacheDir(), "share-test-photo.png");
    }
    @Override public String getType(Uri uri) { file(uri); return "image/png"; }
    @Override public Cursor query(Uri uri, String[] projection, String selection, String[] args, String sort) {
        File file = file(uri);
        String[] columns = projection == null ? new String[]{OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE} : projection;
        MatrixCursor cursor = new MatrixCursor(columns);
        MatrixCursor.RowBuilder row = cursor.newRow();
        for (String column : columns) row.add(OpenableColumns.DISPLAY_NAME.equals(column) ? file.getName() : file.length());
        return cursor;
    }
    @Override public ParcelFileDescriptor openFile(Uri uri, String mode) throws FileNotFoundException {
        if (!"r".equals(mode)) throw new SecurityException("Read only");
        return ParcelFileDescriptor.open(file(uri), ParcelFileDescriptor.MODE_READ_ONLY);
    }
    @Override public Uri insert(Uri uri, ContentValues values) { throw new UnsupportedOperationException(); }
    @Override public int update(Uri uri, ContentValues values, String selection, String[] args) { throw new UnsupportedOperationException(); }
    @Override public int delete(Uri uri, String selection, String[] args) { throw new UnsupportedOperationException(); }
}
