package dev.viso;

import android.animation.ValueAnimator;
import android.app.Activity;
import android.app.UiModeManager;
import android.content.ClipData;
import android.content.ClipDescription;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.content.pm.PackageManager;
import android.content.res.Configuration;
import android.graphics.Bitmap;
import android.graphics.Color;
import android.net.Uri;
import android.hardware.input.InputManager;
import android.os.Build;
import android.os.Bundle;
import android.os.CombinedVibration;
import android.os.VibrationEffect;
import android.os.Vibrator;
import android.os.VibratorManager;
import android.provider.Settings;
import android.util.SparseArray;
import android.view.DisplayCutout;
import android.view.InputDevice;
import android.view.PointerIcon;
import android.view.SurfaceHolder;
import android.view.View;
import android.view.Window;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.WindowManager;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.util.ArrayList;
import java.util.concurrent.CopyOnWriteArrayList;

/**
 * The host activity of a Viso app. It owns one full-screen {@link VisoView}
 * and forwards its surface, input, insets, configuration and lifecycle to the
 * native loop; the native side calls back into the methods marked "native
 * side" below, from its own thread.
 *
 * The native library is named by the {@code dev.viso.lib_name} meta-data of
 * the activity (default {@code main}). It runs the app's {@code main} on a
 * thread of its own once {@link #nativeStart} is called, and keeps running
 * across activity instances until the activity finishes.
 */
public class VisoActivity extends Activity
        implements SurfaceHolder.Callback2, InputManager.InputDeviceListener {
    private static final int APPEARANCE_DARK = 1;
    private static final int APPEARANCE_HIGH_CONTRAST = 2;
    private static final int APPEARANCE_REDUCE_MOTION = 4;

    /**
     * Receives the results of the activities and permission requests others
     * start from the activity. Every receiver sees every result and picks
     * out the request codes it issued.
     */
    public interface Results {
        void onActivityResult(Activity activity, int request, int result, Intent data);

        void onRequestPermissionsResult(Activity activity, int request, String[] permissions,
                int[] grants);
    }

    private static final CopyOnWriteArrayList<Results> results = new CopyOnWriteArrayList<>();

    /** Deliver results to {@code receiver}, from this activity and the ones that replace it. */
    public static void addResults(Results receiver) {
        results.addIfAbsent(receiver);
    }

    private VisoView view;
    private int lastAppearance = -1;
    private float lastDensity = -1;
    private float lastFontScale = -1;
    private String lastLocale = "";
    private int[] lastCutouts = new int[0];
    /** Whether the system bars are hidden; kept across a recreation. */
    private static boolean fullscreen;
    /** Whether the native side reads gamepads; kept across a recreation. */
    static volatile boolean gamepads;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        System.loadLibrary(libraryName());

        Window window = getWindow();
        if (Build.VERSION.SDK_INT >= 30) {
            window.setDecorFitsSystemWindows(false);
        } else {
            window.getDecorView().setSystemUiVisibility(
                    View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                            | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                            | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION);
        }
        window.setStatusBarColor(Color.TRANSPARENT);
        window.setNavigationBarColor(Color.TRANSPARENT);
        if (Build.VERSION.SDK_INT >= 28) {
            WindowManager.LayoutParams attrs = window.getAttributes();
            attrs.layoutInDisplayCutoutMode =
                    WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
            window.setAttributes(attrs);
        }
        window.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE);
        if (fullscreen) {
            applyFullscreen();
        }

        view = new VisoView(this);
        view.getHolder().addCallback(this);
        view.setOnApplyWindowInsetsListener((v, insets) -> {
            reportInsets(insets);
            return insets;
        });
        setContentView(view);
        view.requestFocus();

        Configuration config = getResources().getConfiguration();
        lastDensity = density();
        lastAppearance = appearance(config);
        lastFontScale = config.fontScale;
        lastLocale = locale(config);
        nativeStart(this, lastDensity, lastAppearance, lastFontScale, lastLocale,
                launchExtras());
        if (gamepads) {
            watchGamepads();
        }
        // A recreated activity carries the intent it was first started with,
        // whose link was opened then.
        if (state == null) {
            openLink(getIntent());
        }
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        openLink(intent);
    }

    /**
     * Hand the URL an intent asks the app to view to the native loop: a link
     * of a scheme or host the manifest declares. A relaunch from the recent
     * apps repeats the intent that once started the app, so it opens nothing.
     */
    private static void openLink(Intent intent) {
        if (intent == null || !Intent.ACTION_VIEW.equals(intent.getAction())
                || intent.getData() == null
                || (intent.getFlags() & Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY) != 0) {
            return;
        }
        nativeOpenUrl(intent.getData().toString());
    }

    /**
     * The launch intent's string extras, as names and values in turn: the
     * app's launch variables.
     */
    private String[] launchExtras() {
        Bundle extras = getIntent() == null ? null : getIntent().getExtras();
        if (extras == null) {
            return new String[0];
        }
        java.util.ArrayList<String> pairs = new java.util.ArrayList<>();
        for (String key : extras.keySet()) {
            String value = extras.getString(key);
            if (value != null) {
                pairs.add(key);
                pairs.add(value);
            }
        }
        return pairs.toArray(new String[0]);
    }

    private String libraryName() {
        try {
            ActivityInfo info = getPackageManager()
                    .getActivityInfo(getComponentName(), PackageManager.GET_META_DATA);
            if (info.metaData != null) {
                String name = info.metaData.getString("dev.viso.lib_name");
                if (name != null) {
                    return name;
                }
            }
        } catch (PackageManager.NameNotFoundException ignored) {
        }
        return "main";
    }

    float density() {
        return getResources().getDisplayMetrics().density;
    }

    private int appearance(Configuration config) {
        int bits = 0;
        if ((config.uiMode & Configuration.UI_MODE_NIGHT_MASK) == Configuration.UI_MODE_NIGHT_YES) {
            bits |= APPEARANCE_DARK;
        }
        if (highContrast()) {
            bits |= APPEARANCE_HIGH_CONTRAST;
        }
        if (!ValueAnimator.areAnimatorsEnabled()) {
            bits |= APPEARANCE_REDUCE_MOTION;
        }
        return bits;
    }

    /** The user's first locale, a BCP 47 tag. */
    @SuppressWarnings("deprecation")
    private static String locale(Configuration config) {
        if (Build.VERSION.SDK_INT >= 24) {
            return config.getLocales().get(0).toLanguageTag();
        }
        return config.locale.toLanguageTag();
    }

    private boolean highContrast() {
        if (Build.VERSION.SDK_INT >= 34) {
            UiModeManager modes = (UiModeManager) getSystemService(Context.UI_MODE_SERVICE);
            if (modes != null && modes.getContrast() > 0.5f) {
                return true;
            }
        }
        try {
            return Settings.Secure.getInt(getContentResolver(), "high_text_contrast_enabled", 0) != 0;
        } catch (SecurityException unreadable) {
            return false;
        }
    }

    @SuppressWarnings("deprecation")
    private void reportInsets(WindowInsets insets) {
        int top, left, bottom, right, ime;
        if (Build.VERSION.SDK_INT >= 30) {
            android.graphics.Insets bars = insets.getInsets(
                    WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
            top = bars.top;
            left = bars.left;
            bottom = bars.bottom;
            right = bars.right;
            ime = insets.getInsets(WindowInsets.Type.ime()).bottom;
        } else {
            // Before API 30 the system-window insets include the keyboard;
            // the stable insets do not.
            top = insets.getStableInsetTop();
            left = insets.getStableInsetLeft();
            bottom = insets.getStableInsetBottom();
            right = insets.getStableInsetRight();
            ime = Math.max(0, insets.getSystemWindowInsetBottom() - bottom);
            if (Build.VERSION.SDK_INT >= 28) {
                DisplayCutout cutout = insets.getDisplayCutout();
                if (cutout != null) {
                    top = Math.max(top, cutout.getSafeInsetTop());
                    left = Math.max(left, cutout.getSafeInsetLeft());
                    bottom = Math.max(bottom, cutout.getSafeInsetBottom());
                    right = Math.max(right, cutout.getSafeInsetRight());
                }
            }
        }
        nativeInsets(top, left, bottom, right, ime);
        int[] cutouts = new int[0];
        if (Build.VERSION.SDK_INT >= 28) {
            DisplayCutout cutout = insets.getDisplayCutout();
            if (cutout != null) {
                java.util.List<android.graphics.Rect> rects = cutout.getBoundingRects();
                cutouts = new int[rects.size() * 4];
                for (int i = 0; i < rects.size(); i++) {
                    android.graphics.Rect r = rects.get(i);
                    cutouts[i * 4] = r.left;
                    cutouts[i * 4 + 1] = r.top;
                    cutouts[i * 4 + 2] = r.right;
                    cutouts[i * 4 + 3] = r.bottom;
                }
            }
        }
        if (!java.util.Arrays.equals(cutouts, lastCutouts)) {
            lastCutouts = cutouts;
            nativeCutouts(cutouts);
        }
    }

    /** Report the configuration if anything the native side reads changed. */
    private void reportConfig(Configuration config) {
        int appearance = appearance(config);
        float density = density();
        String locale = locale(config);
        if (appearance != lastAppearance || density != lastDensity
                || config.fontScale != lastFontScale || !locale.equals(lastLocale)) {
            lastAppearance = appearance;
            lastDensity = density;
            lastFontScale = config.fontScale;
            lastLocale = locale;
            nativeConfig(density, appearance, config.fontScale, locale);
        }
    }

    @Override
    public void onConfigurationChanged(Configuration config) {
        super.onConfigurationChanged(config);
        reportConfig(config);
    }

    @Override
    protected void onStart() {
        super.onStart();
        nativeLifecycle(true);
    }

    @Override
    protected void onStop() {
        nativeLifecycle(false);
        super.onStop();
    }

    @Override
    protected void onResume() {
        super.onResume();
        // Accessibility settings have no change broadcast; re-read them when
        // the user comes back, which is where they are changed from.
        reportConfig(getResources().getConfiguration());
    }

    @Override
    public void onWindowFocusChanged(boolean focused) {
        super.onWindowFocusChanged(focused);
        nativeFocus(focused);
    }

    @Override
    public void onTrimMemory(int level) {
        super.onTrimMemory(level);
        if (level >= TRIM_MEMORY_RUNNING_LOW) {
            nativeLowMemory();
        }
    }

    @Override
    public void onLowMemory() {
        super.onLowMemory();
        nativeLowMemory();
    }

    @Override
    protected void onDestroy() {
        super.onDestroy();
        if (gamepads) {
            getSystemService(InputManager.class).unregisterInputDeviceListener(this);
        }
        if (isFinishing()) {
            // The app ends with its activity: stop the loop, then the process,
            // so the next launch starts `main` afresh.
            nativeDestroy();
            System.exit(0);
        }
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        for (Results receiver : results) {
            receiver.onActivityResult(this, request, result, data);
        }
    }

    @Override
    public void onRequestPermissionsResult(int request, String[] permissions, int[] grants) {
        super.onRequestPermissionsResult(request, permissions, grants);
        for (Results receiver : results) {
            receiver.onRequestPermissionsResult(this, request, permissions, grants);
        }
    }

    @Override
    public void surfaceCreated(SurfaceHolder holder) {
        // `surfaceChanged` always follows with the size.
    }

    @Override
    public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) {
        nativeSurfaceChanged(holder.getSurface(), width, height);
    }

    @Override
    public void surfaceDestroyed(SurfaceHolder holder) {
        nativeSurfaceDestroyed();
    }

    @Override
    public void surfaceRedrawNeeded(SurfaceHolder holder) {
        nativeRedraw();
    }

    // Native side: called from the loop thread.

    void setSoftKeyboard(boolean show) {
        runOnUiThread(() -> view.setSoftKeyboard(show));
    }

    void setTextInput(boolean enabled, boolean secure, int kind, boolean multiline, float x,
            float y, float width, float height) {
        runOnUiThread(
                () -> view.setTextInput(enabled, secure, kind, multiline, x, y, width, height));
    }

    void setEditMenu(boolean show, float x, float y, float width, float height, int items) {
        runOnUiThread(() -> view.setEditMenu(show, x, y, width, height, items));
    }

    void setLoupe(boolean show, float x, float y) {
        runOnUiThread(() -> view.setLoupe(show, x, y));
    }

    // Gamepads (native side).

    /** Start reporting the gamepads: those connected now, then each change. */
    void startGamepads() {
        runOnUiThread(() -> {
            if (!gamepads) {
                gamepads = true;
                watchGamepads();
            }
        });
    }

    private void watchGamepads() {
        getSystemService(InputManager.class).registerInputDeviceListener(this, null);
        for (int id : InputDevice.getDeviceIds()) {
            onInputDeviceAdded(id);
        }
    }

    /** Whether {@code device} is a gamepad or joystick. */
    static boolean isGamepad(InputDevice device) {
        int sources = device.getSources();
        return !device.isVirtual()
                && ((sources & InputDevice.SOURCE_GAMEPAD) == InputDevice.SOURCE_GAMEPAD
                        || (sources & InputDevice.SOURCE_JOYSTICK) == InputDevice.SOURCE_JOYSTICK);
    }

    @Override
    public void onInputDeviceAdded(int id) {
        InputDevice device = InputDevice.getDevice(id);
        if (device != null && isGamepad(device)) {
            nativePadAdded(id, device.getName(), hasRumble(device));
        }
    }

    @Override
    public void onInputDeviceRemoved(int id) {
        nativePadRemoved(id);
    }

    @Override
    public void onInputDeviceChanged(int id) {
        onInputDeviceAdded(id);
    }

    @SuppressWarnings("deprecation")
    private static boolean hasRumble(InputDevice device) {
        if (Build.VERSION.SDK_INT >= 31) {
            return device.getVibratorManager().getVibratorIds().length > 0;
        }
        return device.getVibrator().hasVibrator();
    }

    /**
     * Shake gamepad {@code id}: its strong and weak motors (0..1) for {@code ms}
     * milliseconds, or stop it when both are zero; whether it has a motor.
     */
    @SuppressWarnings("deprecation")
    boolean rumbleGamepad(int id, float strong, float weak, int ms) {
        InputDevice device = InputDevice.getDevice(id);
        if (device == null) {
            return false;
        }
        boolean stop = (strong <= 0 && weak <= 0) || ms <= 0;
        if (Build.VERSION.SDK_INT >= 31) {
            VibratorManager manager = device.getVibratorManager();
            int[] motors = manager.getVibratorIds();
            if (motors.length == 0) {
                return false;
            }
            if (stop) {
                manager.cancel();
                return true;
            }
            CombinedVibration.ParallelCombination all = CombinedVibration.startParallel();
            for (int i = 0; i < motors.length; i++) {
                float level = i == 0 ? strong : weak;
                if (motors.length == 1) {
                    level = Math.max(strong, weak);
                }
                int amplitude = Math.max(1, Math.min(255, Math.round(level * 255)));
                all.addVibrator(motors[i], VibrationEffect.createOneShot(ms, amplitude));
            }
            manager.vibrate(all.combine());
            return true;
        }
        Vibrator vibrator = device.getVibrator();
        if (!vibrator.hasVibrator()) {
            return false;
        }
        if (stop) {
            vibrator.cancel();
        } else {
            int amplitude = Math.max(1, Math.min(255, Math.round(Math.max(strong, weak) * 255)));
            vibrator.vibrate(VibrationEffect.createOneShot(ms, amplitude));
        }
        return true;
    }

    void setFullscreen(boolean on) {
        runOnUiThread(() -> {
            fullscreen = on;
            applyFullscreen();
        });
    }

    /**
     * Hide the status and navigation bars, a swipe from an edge showing
     * them for a moment, or show them again.
     */
    @SuppressWarnings("deprecation")
    private void applyFullscreen() {
        Window window = getWindow();
        if (Build.VERSION.SDK_INT >= 30) {
            WindowInsetsController bars = window.getInsetsController();
            if (bars == null) {
                return;
            }
            if (fullscreen) {
                bars.setSystemBarsBehavior(
                        WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE);
                bars.hide(WindowInsets.Type.systemBars());
            } else {
                bars.show(WindowInsets.Type.systemBars());
            }
        } else {
            int layout = View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                    | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                    | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION;
            int hidden = View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
                    | View.SYSTEM_UI_FLAG_FULLSCREEN
                    | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION;
            window.getDecorView().setSystemUiVisibility(fullscreen ? layout | hidden : layout);
        }
    }

    void setPointerIcon(int type) {
        runOnUiThread(() -> view.setPointerIcon(
                type == 0 ? PointerIcon.getSystemIcon(this, PointerIcon.TYPE_NULL)
                        : PointerIcon.getSystemIcon(this, type)));
    }

    /** The pointer icons made from the app's cursor images, by index. */
    private final SparseArray<PointerIcon> imageIcons = new SparseArray<>();

    void setPointerImage(int id, int[] argb, int width, int height, float hotX, float hotY) {
        runOnUiThread(() -> {
            PointerIcon icon = imageIcons.get(id);
            if (icon == null && argb != null) {
                Bitmap bitmap = Bitmap.createBitmap(argb, width, height, Bitmap.Config.ARGB_8888);
                icon = PointerIcon.create(bitmap, hotX, hotY);
                imageIcons.put(id, icon);
            }
            if (icon != null) {
                view.setPointerIcon(icon);
            }
        });
    }

    /** The label of the clip the app last set, to know its own copy. */
    private static volatile String ownClip;

    /**
     * Put one item of {@code text}, {@code html} and a {@code png} image,
     * each optional, on the clipboard under {@code label}. The image is
     * stored here, on the calling thread, and served by
     * {@link VisoClipProvider}.
     */
    void setClipboard(String text, String html, byte[] png, String label) {
        Uri uri = png == null ? null : VisoClipProvider.store(this, png);
        runOnUiThread(() -> {
            ClipboardManager clipboard = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
            if (clipboard == null) {
                return;
            }
            ArrayList<String> types = new ArrayList<>(3);
            if (text != null || html != null) {
                types.add(ClipDescription.MIMETYPE_TEXT_PLAIN);
            }
            if (html != null) {
                types.add(ClipDescription.MIMETYPE_TEXT_HTML);
            }
            if (uri != null) {
                types.add("image/png");
            }
            ClipDescription description = new ClipDescription(label, types.toArray(new String[0]));
            ClipData clip = new ClipData(description, new ClipData.Item(text, html, null, uri));
            ownClip = label;
            try {
                clipboard.setPrimaryClip(clip);
            } catch (RuntimeException e) {
                // A clip too large for the binder: nothing is copied.
            }
        });
    }

    /**
     * Read the clipboard on a thread of its own and answer with
     * {@link #nativePaste}: the item's text and HTML, and its image file's
     * bytes when {@code image} is asked for; {@code own} alone while the
     * clipboard holds the app's last copy.
     */
    void requestClipboard(boolean image, long token) {
        Thread reader = new Thread(() -> readClipboard(image, token), "viso-paste");
        reader.setDaemon(true);
        reader.start();
    }

    private void readClipboard(boolean wantImage, long token) {
        ClipboardManager clipboard = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
        ClipData clip = null;
        try {
            clip = clipboard == null ? null : clipboard.getPrimaryClip();
        } catch (RuntimeException e) {
            // Refused (the app is not in focus): nothing to paste.
        }
        if (clip == null || clip.getItemCount() == 0) {
            nativePaste(token, false, null, null, null);
            return;
        }
        CharSequence label = clip.getDescription().getLabel();
        String own = ownClip;
        if (own != null && label != null && own.contentEquals(label)) {
            nativePaste(token, true, null, null, null);
            return;
        }
        ClipData.Item item = clip.getItemAt(0);
        Uri uri = item.getUri();
        String type = null;
        if (uri != null) {
            try {
                type = getContentResolver().getType(uri);
            } catch (RuntimeException e) {
                type = null;
            }
        }
        boolean isImage = type != null && type.startsWith("image/");
        CharSequence text = item.getText();
        if (text == null && !isImage) {
            text = item.coerceToText(this);
        }
        byte[] bytes = wantImage && isImage ? readAll(uri) : null;
        nativePaste(token, false, text == null ? null : text.toString(), item.getHtmlText(), bytes);
    }

    /** The bytes behind {@code uri}, up to 256 MiB; {@code null} on failure. */
    private byte[] readAll(Uri uri) {
        try (InputStream in = getContentResolver().openInputStream(uri)) {
            if (in == null) {
                return null;
            }
            ByteArrayOutputStream out = new ByteArrayOutputStream(1 << 16);
            byte[] chunk = new byte[1 << 16];
            for (int n; (n = in.read(chunk)) > 0; ) {
                out.write(chunk, 0, n);
                if (out.size() > (256 << 20)) {
                    return null;
                }
            }
            return out.toByteArray();
        } catch (IOException | RuntimeException e) {
            return null;
        }
    }

    void setTitleText(String title) {
        runOnUiThread(() -> setTitle(title));
    }

    void finishFromNative() {
        runOnUiThread(this::finish);
    }

    /** The view assistive technologies read the app's semantics through. */
    View hostView() {
        return view;
    }

    // The native loop.

    static native void nativeStart(VisoActivity activity, float density, int appearance,
            float fontScale, String locale, String[] launchExtras);

    static native void nativeDestroy();

    static native void nativeSurfaceChanged(android.view.Surface surface, int width, int height);

    static native void nativeSurfaceDestroyed();

    static native void nativeRedraw();

    static native void nativeLifecycle(boolean visible);

    static native void nativeFocus(boolean focused);

    static native void nativeInsets(int top, int left, int bottom, int right, int ime);

    static native void nativeConfig(float density, int appearance, float fontScale,
            String locale);

    /** The display cutouts' bounding rects, four ints (left, top, right, bottom) each, in px. */
    static native void nativeCutouts(int[] rects);

    static native void nativeLowMemory();

    static native void nativeTouch(int action, int actionIndex, int[] ids, float[] samples,
            int[] tools, int buttons, int meta);

    static native void nativeScroll(float x, float y, float dx, float dy, int meta);

    static native void nativeKey(int keyCode, boolean pressed, boolean repeat, int meta, String text);

    static native void nativePadAdded(int id, String name, boolean rumble);

    static native void nativePadRemoved(int id);

    static native void nativePadKey(int id, int keyCode, boolean pressed);

    /**
     * A gamepad's axes: the sticks (x, y, z, rz), the hat (x, y) and the
     * triggers (left, right, brake, gas).
     */
    static native void nativePadMotion(int id, float x, float y, float z, float rz, float hatX,
            float hatY, float left, float right, float brake, float gas);

    static native void nativePreedit(String text, int caret);

    static native void nativeCommit(String text);

    static native void nativeEdit(int action);

    static native void nativeOpenUrl(String url);

    /**
     * A step of a drag from another app: over the view at ({@code x}, {@code y}) in px, offering
     * {@code kinds}, or leaving it. With {@code answer} the call waits for the app and returns
     * what it would do with a drop there, 0 for nothing; otherwise the app's last answer.
     */
    static native int nativeDrag(int phase, float x, float y, int kinds, boolean answer);

    /** The drag was let go at ({@code x}, {@code y}) in px, holding these. */
    static native void nativeDrop(float x, float y, String text, String link, String[] files);

    /**
     * The clipboard read for {@code token} ({@code -1}: a paste gesture):
     * the app's own copy, or what it holds, any part {@code null}.
     */
    static native void nativePaste(long token, boolean own, String text, String html, byte[] image);
}
