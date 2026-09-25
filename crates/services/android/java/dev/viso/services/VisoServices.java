package dev.viso.services;

import android.app.Activity;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.ActivityNotFoundException;
import android.content.ClipData;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.database.Cursor;
import android.net.Uri;
import android.os.Build;
import android.os.Vibrator;
import android.os.VibratorManager;
import android.provider.OpenableColumns;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.util.Base64;
import android.view.HapticFeedbackConstants;
import android.view.View;
import android.webkit.MimeTypeMap;

import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.security.KeyStore;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;

import dev.viso.VisoActivity;

/**
 * The Android side of {@code viso-services}. The native side calls the
 * static methods below from the app's thread, each with a token; the answer
 * comes back through one of the {@code native*} callbacks, carrying that
 * token, from whichever thread finished the work. Activity work runs on the
 * UI thread; file and keystore I/O on worker threads.
 */
public final class VisoServices {
    // The outcome of a request, as the native side reads it.
    static final int OK = 0;
    static final int CANCELLED = 1;
    static final int DENIED = 2;
    static final int FAILED = 3;
    static final int UNSUPPORTED = 4;

    // Permission states.
    static final int GRANTED = 0;
    static final int REFUSED = 1;
    static final int PROMPT = 2;

    private static final String NOTIFICATIONS_PERMISSION = "android.permission.POST_NOTIFICATIONS";
    private static final String CHANNEL = "viso";
    private static final String KEY_ALIAS = "dev.viso.secure-storage";
    private static final String PREFERENCES = "dev.viso.services";
    private static final String SECRETS = "dev.viso.secure-storage";
    private static final int IV_BYTES = 12;
    private static final int TAG_BITS = 128;

    // Request codes, kept within 16 bits for the hosts that require it.
    private static final int FIRST_REQUEST = 0x5600;
    private static final int REQUESTS = 0x100;

    private static final int OPEN = 0;
    private static final int SAVE = 1;
    private static final int PERMISSION = 2;

    /** A request waiting on its activity or permission result. UI thread only. */
    private static final class Request {
        final long token;
        final int kind;
        final byte[] contents;

        Request(long token, int kind, byte[] contents) {
            this.token = token;
            this.kind = kind;
            this.contents = contents;
        }
    }

    private static final Map<Integer, Request> requests = new HashMap<>();
    private static int nextRequest;
    private static boolean listening;

    private static final ExecutorService files = Executors.newCachedThreadPool();
    // Secrets are read and written in the order they were asked for.
    private static final ExecutorService secrets = Executors.newSingleThreadExecutor();

    private VisoServices() {
    }

    // File dialogs.

    static void openDocument(Activity activity, long token, String[] extensions, boolean multiple) {
        activity.runOnUiThread(() -> {
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            intent.setType("*/*");
            String[] types = mimeTypes(extensions);
            if (types.length > 0) {
                intent.putExtra(Intent.EXTRA_MIME_TYPES, types);
            }
            intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, multiple);
            start(activity, new Request(token, OPEN, null), intent);
        });
    }

    static void createDocument(Activity activity, long token, String name, String extension,
            byte[] contents) {
        activity.runOnUiThread(() -> {
            Intent intent = new Intent(Intent.ACTION_CREATE_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            String type = mimeType(extension.isEmpty() ? extensionOf(name) : extension);
            intent.setType(type == null ? "application/octet-stream" : type);
            intent.putExtra(Intent.EXTRA_TITLE, name);
            start(activity, new Request(token, SAVE, contents), intent);
        });
    }

    /** The MIME types of {@code extensions}; none (any file) if one has no known type. */
    private static String[] mimeTypes(String[] extensions) {
        List<String> types = new ArrayList<>();
        for (String extension : extensions) {
            String type = mimeType(extension);
            if (type == null) {
                return new String[0];
            }
            if (!types.contains(type)) {
                types.add(type);
            }
        }
        return types.toArray(new String[0]);
    }

    private static String mimeType(String extension) {
        if (extension.isEmpty()) {
            return null;
        }
        return MimeTypeMap.getSingleton().getMimeTypeFromExtension(extension.toLowerCase());
    }

    private static String extensionOf(String name) {
        int dot = name.lastIndexOf('.');
        return dot < 0 ? "" : name.substring(dot + 1);
    }

    private static void start(Activity activity, Request request, Intent intent) {
        listen();
        int code = FIRST_REQUEST + nextRequest;
        nextRequest = (nextRequest + 1) % REQUESTS;
        requests.put(code, request);
        try {
            activity.startActivityForResult(intent, code);
        } catch (ActivityNotFoundException missing) {
            requests.remove(code);
            answer(request, UNSUPPORTED, null);
        }
    }

    private static void listen() {
        if (listening) {
            return;
        }
        listening = true;
        VisoActivity.addResults(new VisoActivity.Results() {
            @Override
            public void onActivityResult(Activity activity, int code, int result, Intent data) {
                Request request = requests.remove(code);
                if (request == null) {
                    return;
                }
                if (result != Activity.RESULT_OK || data == null) {
                    answer(request, CANCELLED, null);
                } else if (request.kind == OPEN) {
                    read(activity.getApplicationContext(), request.token, picked(data));
                } else if (request.kind == SAVE) {
                    write(activity.getApplicationContext(), request, data.getData());
                }
            }

            @Override
            public void onRequestPermissionsResult(Activity activity, int code,
                    String[] permissions, int[] grants) {
                Request request = requests.remove(code);
                if (request == null) {
                    return;
                }
                if (grants.length == 0) {
                    nativeState(request.token, CANCELLED, 0);
                } else {
                    boolean granted = grants[0] == PackageManager.PERMISSION_GRANTED;
                    nativeState(request.token, OK, granted ? GRANTED : REFUSED);
                }
            }
        });
    }

    private static void answer(Request request, int status, String message) {
        if (request.kind == OPEN) {
            nativeFiles(request.token, status, message, null, null);
        } else if (request.kind == SAVE) {
            nativeDone(request.token, status, message);
        } else {
            nativeState(request.token, status, 0);
        }
    }

    private static List<Uri> picked(Intent data) {
        List<Uri> uris = new ArrayList<>();
        ClipData clip = data.getClipData();
        if (clip != null) {
            for (int i = 0; i < clip.getItemCount(); i++) {
                uris.add(clip.getItemAt(i).getUri());
            }
        } else if (data.getData() != null) {
            uris.add(data.getData());
        }
        return uris;
    }

    private static void read(Context context, long token, List<Uri> uris) {
        files.execute(() -> {
            String[] names = new String[uris.size()];
            byte[][] contents = new byte[uris.size()][];
            try {
                for (int i = 0; i < uris.size(); i++) {
                    names[i] = displayName(context, uris.get(i));
                    contents[i] = readAll(context, uris.get(i));
                }
            } catch (Exception e) {
                nativeFiles(token, FAILED, e.toString(), null, null);
                return;
            }
            nativeFiles(token, OK, null, names, contents);
        });
    }

    private static String displayName(Context context, Uri uri) {
        String[] columns = {OpenableColumns.DISPLAY_NAME};
        try (Cursor cursor = context.getContentResolver().query(uri, columns, null, null, null)) {
            if (cursor != null && cursor.moveToFirst() && !cursor.isNull(0)) {
                return cursor.getString(0);
            }
        }
        String last = uri.getLastPathSegment();
        return last == null ? "" : last;
    }

    private static byte[] readAll(Context context, Uri uri) throws Exception {
        try (InputStream in = context.getContentResolver().openInputStream(uri)) {
            if (in == null) {
                throw new java.io.FileNotFoundException(uri.toString());
            }
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            byte[] buffer = new byte[64 * 1024];
            for (int n; (n = in.read(buffer)) != -1; ) {
                out.write(buffer, 0, n);
            }
            return out.toByteArray();
        }
    }

    private static void write(Context context, Request request, Uri uri) {
        if (uri == null) {
            nativeDone(request.token, CANCELLED, null);
            return;
        }
        files.execute(() -> {
            try (OutputStream out = context.getContentResolver().openOutputStream(uri, "wt")) {
                if (out == null) {
                    throw new java.io.FileNotFoundException(uri.toString());
                }
                out.write(request.contents);
            } catch (Exception e) {
                nativeDone(request.token, FAILED, e.toString());
                return;
            }
            nativeDone(request.token, OK, null);
        });
    }

    // Share.

    static void share(Activity activity, long token, String text) {
        activity.runOnUiThread(() -> {
            Intent send = new Intent(Intent.ACTION_SEND);
            send.setType("text/plain");
            send.putExtra(Intent.EXTRA_TEXT, text);
            try {
                activity.startActivity(Intent.createChooser(send, null));
            } catch (ActivityNotFoundException missing) {
                nativeDone(token, UNSUPPORTED, null);
                return;
            }
            nativeDone(token, OK, null);
        });
    }

    // Notifications.

    static void notify(Activity activity, long token, String id, String title, String body) {
        Context context = activity.getApplicationContext();
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager == null) {
            nativeDone(token, UNSUPPORTED, null);
            return;
        }
        if (!manager.areNotificationsEnabled()) {
            nativeDone(token, DENIED, null);
            return;
        }
        manager.createNotificationChannel(new NotificationChannel(
                CHANNEL, "Notifications", NotificationManager.IMPORTANCE_DEFAULT));
        Intent open = new Intent(context, activity.getClass())
                .setFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_SINGLE_TOP);
        PendingIntent tap = PendingIntent.getActivity(context, 0, open,
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        int icon = context.getApplicationInfo().icon;
        Notification notification = new Notification.Builder(context, CHANNEL)
                .setSmallIcon(icon != 0 ? icon : android.R.drawable.ic_dialog_info)
                .setContentTitle(title)
                .setContentText(body)
                .setContentIntent(tap)
                .setAutoCancel(true)
                .build();
        try {
            manager.notify(id, 0, notification);
        } catch (SecurityException refused) {
            nativeDone(token, DENIED, null);
            return;
        }
        nativeDone(token, OK, null);
    }

    static void withdraw(Activity activity, String id) {
        NotificationManager manager =
                activity.getApplicationContext().getSystemService(NotificationManager.class);
        if (manager != null) {
            manager.cancel(id, 0);
        }
    }

    // Permissions.

    static int permissionStatus(Activity activity) {
        if (Build.VERSION.SDK_INT < 33) {
            NotificationManager manager = activity.getSystemService(NotificationManager.class);
            return manager != null && manager.areNotificationsEnabled() ? GRANTED : REFUSED;
        }
        if (activity.checkSelfPermission(NOTIFICATIONS_PERMISSION)
                == PackageManager.PERMISSION_GRANTED) {
            return GRANTED;
        }
        // The system asks again after one refusal, then no more.
        if (!preferences(activity).getBoolean(NOTIFICATIONS_PERMISSION, false)
                || activity.shouldShowRequestPermissionRationale(NOTIFICATIONS_PERMISSION)) {
            return PROMPT;
        }
        return REFUSED;
    }

    static void requestPermission(Activity activity, long token) {
        activity.runOnUiThread(() -> {
            int status = permissionStatus(activity);
            if (status != PROMPT) {
                nativeState(token, OK, status);
                return;
            }
            preferences(activity).edit().putBoolean(NOTIFICATIONS_PERMISSION, true).apply();
            listen();
            int code = FIRST_REQUEST + nextRequest;
            nextRequest = (nextRequest + 1) % REQUESTS;
            requests.put(code, new Request(token, PERMISSION, null));
            activity.requestPermissions(new String[] {NOTIFICATIONS_PERMISSION}, code);
        });
    }

    private static SharedPreferences preferences(Context context) {
        return context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE);
    }

    // Secure storage: each value sealed with an AES-GCM key the Android
    // keystore holds, the sealed bytes in the app's private preferences.

    static void secretGet(Activity activity, long token, String key) {
        Context context = activity.getApplicationContext();
        secrets.execute(() -> {
            byte[] value;
            try {
                String sealed = context.getSharedPreferences(SECRETS, Context.MODE_PRIVATE)
                        .getString(key, null);
                value = sealed == null ? null : open(key, Base64.decode(sealed, Base64.NO_WRAP));
            } catch (Exception e) {
                nativeBytes(token, FAILED, e.toString(), null);
                return;
            }
            nativeBytes(token, OK, null, value);
        });
    }

    static void secretSet(Activity activity, long token, String key, byte[] value) {
        Context context = activity.getApplicationContext();
        secrets.execute(() -> {
            try {
                String sealed = Base64.encodeToString(seal(key, value), Base64.NO_WRAP);
                boolean saved = context.getSharedPreferences(SECRETS, Context.MODE_PRIVATE)
                        .edit().putString(key, sealed).commit();
                if (!saved) {
                    nativeDone(token, FAILED, "the value could not be written");
                    return;
                }
            } catch (Exception e) {
                nativeDone(token, FAILED, e.toString());
                return;
            }
            nativeDone(token, OK, null);
        });
    }

    static void secretRemove(Activity activity, long token, String key) {
        Context context = activity.getApplicationContext();
        secrets.execute(() -> {
            boolean removed = context.getSharedPreferences(SECRETS, Context.MODE_PRIVATE)
                    .edit().remove(key).commit();
            nativeDone(token, removed ? OK : FAILED, removed ? null : "the value could not be removed");
        });
    }

    private static SecretKey secretKey() throws Exception {
        KeyStore store = KeyStore.getInstance("AndroidKeyStore");
        store.load(null);
        java.security.Key existing = store.getKey(KEY_ALIAS, null);
        if (existing instanceof SecretKey) {
            return (SecretKey) existing;
        }
        KeyGenerator generator =
                KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
        generator.init(new KeyGenParameterSpec.Builder(KEY_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build());
        return generator.generateKey();
    }

    /** The IV, then `value` encrypted and bound to `key`. */
    private static byte[] seal(String key, byte[] value) throws Exception {
        Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
        cipher.init(Cipher.ENCRYPT_MODE, secretKey());
        cipher.updateAAD(key.getBytes(StandardCharsets.UTF_8));
        byte[] iv = cipher.getIV();
        byte[] sealed = cipher.doFinal(value);
        byte[] out = new byte[iv.length + sealed.length];
        System.arraycopy(iv, 0, out, 0, iv.length);
        System.arraycopy(sealed, 0, out, iv.length, sealed.length);
        return out;
    }

    private static byte[] open(String key, byte[] sealed) throws Exception {
        Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
        cipher.init(Cipher.DECRYPT_MODE, secretKey(),
                new GCMParameterSpec(TAG_BITS, sealed, 0, IV_BYTES));
        cipher.updateAAD(key.getBytes(StandardCharsets.UTF_8));
        return cipher.doFinal(sealed, IV_BYTES, sealed.length - IV_BYTES);
    }

    // Haptics, through the view's haptic feedback: it follows the user's
    // touch-feedback setting and needs no permission.

    static int vibrate(Activity activity, int haptic) {
        if (!hasVibrator(activity)) {
            return UNSUPPORTED;
        }
        int feedback = feedback(haptic);
        activity.runOnUiThread(() -> {
            View view = activity.getWindow().getDecorView();
            view.performHapticFeedback(feedback);
        });
        return OK;
    }

    private static int feedback(int haptic) {
        boolean api30 = Build.VERSION.SDK_INT >= 30;
        switch (haptic) {
            case 0:
                return HapticFeedbackConstants.CLOCK_TICK;
            case 1:
                return HapticFeedbackConstants.KEYBOARD_TAP;
            case 2:
                return HapticFeedbackConstants.CONTEXT_CLICK;
            case 3:
                return HapticFeedbackConstants.LONG_PRESS;
            case 4:
                return api30 ? HapticFeedbackConstants.CONFIRM : HapticFeedbackConstants.CONTEXT_CLICK;
            case 5:
                return HapticFeedbackConstants.LONG_PRESS;
            default:
                return api30 ? HapticFeedbackConstants.REJECT : HapticFeedbackConstants.LONG_PRESS;
        }
    }

    @SuppressWarnings("deprecation")
    private static boolean hasVibrator(Activity activity) {
        Vibrator vibrator;
        if (Build.VERSION.SDK_INT >= 31) {
            VibratorManager manager = activity.getSystemService(VibratorManager.class);
            vibrator = manager == null ? null : manager.getDefaultVibrator();
        } else {
            vibrator = (Vibrator) activity.getSystemService(Context.VIBRATOR_SERVICE);
        }
        return vibrator != null && vibrator.hasVibrator();
    }

    // The native side's answers.

    static native void nativeFiles(long token, int status, String message, String[] names,
            byte[][] contents);

    static native void nativeBytes(long token, int status, String message, byte[] value);

    static native void nativeDone(long token, int status, String message);

    static native void nativeState(long token, int status, int state);
}
