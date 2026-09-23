package dev.viso;

import android.animation.ValueAnimator;
import android.app.Activity;
import android.app.UiModeManager;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.pm.ActivityInfo;
import android.content.pm.PackageManager;
import android.content.res.Configuration;
import android.graphics.Color;
import android.os.Build;
import android.os.Bundle;
import android.provider.Settings;
import android.view.DisplayCutout;
import android.view.PointerIcon;
import android.view.SurfaceHolder;
import android.view.View;
import android.view.Window;
import android.view.WindowInsets;
import android.view.WindowManager;

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
public class VisoActivity extends Activity implements SurfaceHolder.Callback2 {
    private static final int APPEARANCE_DARK = 1;
    private static final int APPEARANCE_HIGH_CONTRAST = 2;
    private static final int APPEARANCE_REDUCE_MOTION = 4;

    private VisoView view;
    private int lastAppearance = -1;
    private float lastDensity = -1;

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

        view = new VisoView(this);
        view.getHolder().addCallback(this);
        view.setOnApplyWindowInsetsListener((v, insets) -> {
            reportInsets(insets);
            return insets;
        });
        setContentView(view);
        view.requestFocus();

        lastDensity = density();
        lastAppearance = appearance(getResources().getConfiguration());
        nativeStart(this, lastDensity, lastAppearance);
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
    }

    @Override
    public void onConfigurationChanged(Configuration config) {
        super.onConfigurationChanged(config);
        int appearance = appearance(config);
        float density = density();
        if (appearance != lastAppearance || density != lastDensity) {
            lastAppearance = appearance;
            lastDensity = density;
            nativeConfig(density, appearance);
        }
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
        int appearance = appearance(getResources().getConfiguration());
        if (appearance != lastAppearance) {
            lastAppearance = appearance;
            nativeConfig(density(), appearance);
        }
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
        if (isFinishing()) {
            // The app ends with its activity: stop the loop, then the process,
            // so the next launch starts `main` afresh.
            nativeDestroy();
            System.exit(0);
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

    void setTextInput(boolean enabled, float x, float y, float width, float height) {
        runOnUiThread(() -> view.setTextInput(enabled, x, y, width, height));
    }

    void setPointerIcon(int type) {
        runOnUiThread(() -> view.setPointerIcon(
                type == 0 ? PointerIcon.getSystemIcon(this, PointerIcon.TYPE_NULL)
                        : PointerIcon.getSystemIcon(this, type)));
    }

    void setClipboard(String text) {
        runOnUiThread(() -> {
            ClipboardManager clipboard = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
            if (clipboard != null) {
                clipboard.setPrimaryClip(ClipData.newPlainText("text", text));
            }
        });
    }

    String getClipboard() {
        ClipboardManager clipboard = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
        if (clipboard == null || !clipboard.hasPrimaryClip()) {
            return null;
        }
        ClipData clip = clipboard.getPrimaryClip();
        if (clip == null || clip.getItemCount() == 0) {
            return null;
        }
        CharSequence text = clip.getItemAt(0).coerceToText(this);
        return text == null ? null : text.toString();
    }

    void setTitleText(String title) {
        runOnUiThread(() -> setTitle(title));
    }

    void finishFromNative() {
        runOnUiThread(this::finish);
    }

    // The native loop.

    static native void nativeStart(VisoActivity activity, float density, int appearance);

    static native void nativeDestroy();

    static native void nativeSurfaceChanged(android.view.Surface surface, int width, int height);

    static native void nativeSurfaceDestroyed();

    static native void nativeRedraw();

    static native void nativeLifecycle(boolean visible);

    static native void nativeFocus(boolean focused);

    static native void nativeInsets(int top, int left, int bottom, int right, int ime);

    static native void nativeConfig(float density, int appearance);

    static native void nativeLowMemory();

    static native void nativeTouch(int action, int actionIndex, int[] ids, float[] samples,
            int[] tools, int buttons, int meta);

    static native void nativeScroll(float x, float y, float dx, float dy, int meta);

    static native void nativeKey(int keyCode, boolean pressed, boolean repeat, int meta, String text);

    static native void nativePreedit(String text, int caret);

    static native void nativeCommit(String text);

    static native void nativeEdit(int action);
}
