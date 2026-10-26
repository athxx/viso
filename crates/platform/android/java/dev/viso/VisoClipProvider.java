package dev.viso;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.content.Context;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;

import java.io.File;
import java.io.FileNotFoundException;
import java.io.FileOutputStream;
import java.io.IOException;

/**
 * Serves the image the app last copied, a PNG file in the cache directory,
 * to whichever app pastes it. Readers get access through the clipboard's
 * URI grant; nothing else is served, and nothing can be written.
 */
public final class VisoClipProvider extends ContentProvider {
    private static final String DIR = "viso-clipboard";

    static String authority(Context context) {
        return context.getPackageName() + ".viso.clipboard";
    }

    /**
     * Store {@code png} as the copied image, replacing the last one, and
     * return its URI; {@code null} if it could not be written.
     */
    static Uri store(Context context, byte[] png) {
        File dir = new File(context.getCacheDir(), DIR);
        if (!dir.isDirectory() && !dir.mkdirs()) {
            return null;
        }
        File[] old = dir.listFiles();
        if (old != null) {
            for (File f : old) {
                f.delete();
            }
        }
        String name = "image-" + System.nanoTime() + ".png";
        File file = new File(dir, name);
        try (FileOutputStream out = new FileOutputStream(file)) {
            out.write(png);
        } catch (IOException e) {
            file.delete();
            return null;
        }
        return new Uri.Builder()
                .scheme("content")
                .authority(authority(context))
                .appendPath(name)
                .build();
    }

    private File fileOf(Uri uri) {
        String name = uri.getLastPathSegment();
        Context context = getContext();
        if (context == null || name == null || name.contains("/") || name.startsWith(".")) {
            return null;
        }
        File file = new File(new File(context.getCacheDir(), DIR), name);
        return file.isFile() ? file : null;
    }

    @Override
    public boolean onCreate() {
        return true;
    }

    @Override
    public String getType(Uri uri) {
        return "image/png";
    }

    @Override
    public ParcelFileDescriptor openFile(Uri uri, String mode) throws FileNotFoundException {
        File file = fileOf(uri);
        if (file == null || !"r".equals(mode)) {
            throw new FileNotFoundException(uri.toString());
        }
        return ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY);
    }

    @Override
    public Cursor query(Uri uri, String[] projection, String selection, String[] args,
            String order) {
        File file = fileOf(uri);
        if (file == null) {
            return null;
        }
        String[] columns = {OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE};
        MatrixCursor cursor = new MatrixCursor(columns, 1);
        cursor.addRow(new Object[] {file.getName(), file.length()});
        return cursor;
    }

    @Override
    public Uri insert(Uri uri, ContentValues values) {
        return null;
    }

    @Override
    public int delete(Uri uri, String selection, String[] args) {
        return 0;
    }

    @Override
    public int update(Uri uri, ContentValues values, String selection, String[] args) {
        return 0;
    }
}
