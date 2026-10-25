package dev.viso.services;

import android.app.Activity;
import android.content.ActivityNotFoundException;
import android.content.Context;
import android.content.Intent;
import android.net.Uri;
import android.os.Build;
import android.os.Vibrator;
import android.os.VibratorManager;
import android.view.HapticFeedbackConstants;
import android.view.View;

/**
 * The Android side of {@code viso-services}. The native side calls the
 * static methods below from the app's thread; an asynchronous answer comes
 * back through {@code nativeDone}, carrying the call's token. Activity work
 * runs on the UI thread.
 */
public final class VisoServices {
    // The outcome of a request, as the native side reads it.
    static final int OK = 0;
    static final int UNSUPPORTED = 4;

    private VisoServices() {
    }

    // Opening URLs: an app that takes browsable links of the URL's scheme.

    static void openUrl(Activity activity, long token, String url) {
        activity.runOnUiThread(() -> {
            Intent view = new Intent(Intent.ACTION_VIEW, Uri.parse(url));
            view.addCategory(Intent.CATEGORY_BROWSABLE);
            try {
                activity.startActivity(view);
            } catch (ActivityNotFoundException missing) {
                nativeDone(token, UNSUPPORTED, null);
                return;
            }
            nativeDone(token, OK, null);
        });
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

    // The native side's answer.

    static native void nativeDone(long token, int status, String message);
}
