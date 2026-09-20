package org.ratspeak.android;

import android.app.Activity;
import android.content.ClipData;
import android.content.Intent;
import android.graphics.Bitmap;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import java.io.File;
import java.io.FileOutputStream;
import java.util.Random;

/** Separate-UID source for offline AVD integration only; platform Java so the
 * test APK can launch independently of the instrumented app's class loader. */
public class ShareSourceActivity extends Activity {
    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        if (!(Build.HARDWARE.equals("ranchu") || Build.HARDWARE.equals("goldfish"))
                || !getPackageName().equals("org.ratspeak.android.test")) throw new SecurityException("Emulator test APK only");
        File file = new File(getCacheDir(), "share-test-photo.png");
        try {
            if (!file.exists()) {
                Random random = new Random(19);
                int[] pixels = new int[1024 * 1024];
                for (int i = 0; i < pixels.length; i++) pixels[i] = random.nextInt() | 0xff000000;
                Bitmap bitmap = Bitmap.createBitmap(pixels, 1024, 1024, Bitmap.Config.ARGB_8888);
                try (FileOutputStream out = new FileOutputStream(file)) {
                    if (!bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)) throw new IllegalStateException("Fixture failed");
                } finally { bitmap.recycle(); }
            }
        } catch (java.io.IOException error) { throw new IllegalStateException(error); }
        Uri uri = ShareSourceProvider.URI;
        Intent share = new Intent(Intent.ACTION_SEND).setType("image/png")
            .setClassName("org.ratspeak.android", "org.ratspeak.android.MainActivity")
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        share.setClipData(ClipData.newRawUri("Synthetic share photo", uri));
        if (!getIntent().getBooleanExtra("clipOnly", false)) share.putExtra(Intent.EXTRA_STREAM, uri);
        if (getIntent().getBooleanExtra("caption", false)) share.putExtra(Intent.EXTRA_TEXT, "Photo share acceptance probe");
        startActivity(share);
        finish();
    }
}
