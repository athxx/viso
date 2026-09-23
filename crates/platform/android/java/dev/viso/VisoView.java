package dev.viso;

import android.content.Context;
import android.graphics.Matrix;
import android.os.Build;
import android.view.InputDevice;
import android.view.KeyCharacterMap;
import android.view.KeyEvent;
import android.view.MotionEvent;
import android.view.SurfaceView;
import android.view.ViewConfiguration;
import android.view.inputmethod.CursorAnchorInfo;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;

/**
 * The surface a Viso app draws into, and the receiver of its touch, mouse,
 * stylus, key and input-method traffic.
 */
final class VisoView extends SurfaceView {
    private final InputMethodManager imm;
    private final float scrollFactorX;
    private final float scrollFactorY;
    private VisoInputConnection connection;
    private boolean textInput;
    private float caretX, caretY, caretWidth, caretHeight;
    private boolean cursorUpdates;
    private int deadKey;

    VisoView(Context context) {
        super(context);
        imm = (InputMethodManager) context.getSystemService(Context.INPUT_METHOD_SERVICE);
        ViewConfiguration config = ViewConfiguration.get(context);
        scrollFactorX = config.getScaledHorizontalScrollFactor();
        scrollFactorY = config.getScaledVerticalScrollFactor();
        setFocusable(true);
        setFocusableInTouchMode(true);
    }

    // Pointers.

    @Override
    public boolean onTouchEvent(MotionEvent event) {
        sendMotion(event);
        return true;
    }

    @Override
    public boolean onGenericMotionEvent(MotionEvent event) {
        if (!event.isFromSource(InputDevice.SOURCE_CLASS_POINTER)) {
            return super.onGenericMotionEvent(event);
        }
        if (event.getActionMasked() == MotionEvent.ACTION_SCROLL) {
            VisoActivity.nativeScroll(event.getX(), event.getY(),
                    -event.getAxisValue(MotionEvent.AXIS_HSCROLL) * scrollFactorX,
                    event.getAxisValue(MotionEvent.AXIS_VSCROLL) * scrollFactorY,
                    event.getMetaState());
        } else {
            sendMotion(event);
        }
        return true;
    }

    private void sendMotion(MotionEvent event) {
        int count = event.getPointerCount();
        int[] ids = new int[count];
        int[] tools = new int[count];
        float[] samples = new float[count * 3];
        for (int i = 0; i < count; i++) {
            ids[i] = event.getPointerId(i);
            tools[i] = event.getToolType(i);
        }
        int action = event.getActionMasked();
        // Coalesced moves: replay the batched history first, oldest first, so
        // strokes keep every sample the digitizer produced.
        if (action == MotionEvent.ACTION_MOVE) {
            for (int h = 0; h < event.getHistorySize(); h++) {
                for (int i = 0; i < count; i++) {
                    samples[i * 3] = event.getHistoricalX(i, h);
                    samples[i * 3 + 1] = event.getHistoricalY(i, h);
                    samples[i * 3 + 2] = event.getHistoricalPressure(i, h);
                }
                VisoActivity.nativeTouch(action, 0, ids, samples, tools,
                        event.getButtonState(), event.getMetaState());
            }
        }
        for (int i = 0; i < count; i++) {
            samples[i * 3] = event.getX(i);
            samples[i * 3 + 1] = event.getY(i);
            samples[i * 3 + 2] = event.getPressure(i);
        }
        VisoActivity.nativeTouch(action, event.getActionIndex(), ids, samples, tools,
                event.getButtonState(), event.getMetaState());
    }

    // Keys.

    @Override
    public boolean onKeyDown(int keyCode, KeyEvent event) {
        return sendKey(keyCode, event, true);
    }

    @Override
    public boolean onKeyUp(int keyCode, KeyEvent event) {
        return sendKey(keyCode, event, false);
    }

    @Override
    public boolean onKeyMultiple(int keyCode, int repeatCount, KeyEvent event) {
        String chars = event.getCharacters();
        if (keyCode == KeyEvent.KEYCODE_UNKNOWN && chars != null) {
            VisoActivity.nativeCommit(chars);
            return true;
        }
        return super.onKeyMultiple(keyCode, repeatCount, event);
    }

    private boolean sendKey(int keyCode, KeyEvent event, boolean pressed) {
        int meta = event.getMetaState();
        String text = null;
        if (pressed) {
            int unicode = event.getUnicodeChar(meta);
            if ((unicode & KeyCharacterMap.COMBINING_ACCENT) != 0) {
                deadKey = unicode & KeyCharacterMap.COMBINING_ACCENT_MASK;
                unicode = 0;
            } else if (deadKey != 0 && unicode != 0) {
                int composed = KeyEvent.getDeadChar(deadKey, unicode);
                deadKey = 0;
                if (composed != 0) {
                    unicode = composed;
                }
            }
            if (unicode != 0 && !Character.isISOControl(unicode)) {
                text = new String(Character.toChars(unicode));
            }
        }
        VisoActivity.nativeKey(keyCode, pressed, event.getRepeatCount() > 0, meta, text);
        // The system keeps back, volume and media keys: the app hears them
        // but does not consume them.
        switch (keyCode) {
            case KeyEvent.KEYCODE_BACK:
            case KeyEvent.KEYCODE_VOLUME_UP:
            case KeyEvent.KEYCODE_VOLUME_DOWN:
            case KeyEvent.KEYCODE_VOLUME_MUTE:
            case KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE:
            case KeyEvent.KEYCODE_MEDIA_STOP:
            case KeyEvent.KEYCODE_MEDIA_NEXT:
            case KeyEvent.KEYCODE_MEDIA_PREVIOUS:
                return false;
            default:
                return true;
        }
    }

    /** Send a key tap an input method spelled as an edit. */
    void tapKey(int keyCode) {
        VisoActivity.nativeKey(keyCode, true, false, 0, null);
        VisoActivity.nativeKey(keyCode, false, false, 0, null);
    }

    // Input methods.

    @Override
    public boolean onCheckIsTextEditor() {
        return textInput;
    }

    @Override
    public InputConnection onCreateInputConnection(EditorInfo info) {
        if (!textInput) {
            return null;
        }
        info.inputType = EditorInfo.TYPE_CLASS_TEXT | EditorInfo.TYPE_TEXT_FLAG_MULTI_LINE;
        info.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN | EditorInfo.IME_FLAG_NO_EXTRACT_UI
                | EditorInfo.IME_ACTION_NONE;
        info.initialSelStart = 0;
        info.initialSelEnd = 0;
        connection = new VisoInputConnection(this);
        return connection;
    }

    void setTextInput(boolean enabled, float x, float y, float width, float height) {
        caretX = x;
        caretY = y;
        caretWidth = width;
        caretHeight = height;
        if (enabled != textInput) {
            textInput = enabled;
            if (!enabled && connection != null) {
                connection.cancelComposition();
            }
            connection = null;
            imm.restartInput(this);
            if (!enabled) {
                imm.hideSoftInputFromWindow(getWindowToken(), 0);
            }
        }
        if (enabled && cursorUpdates) {
            reportCursor();
        }
    }

    void setSoftKeyboard(boolean show) {
        if (show) {
            requestFocus();
            imm.showSoftInput(this, 0);
        } else {
            imm.hideSoftInputFromWindow(getWindowToken(), 0);
        }
    }

    void setCursorUpdates(boolean enabled) {
        cursorUpdates = enabled;
        if (enabled && textInput) {
            reportCursor();
        }
    }

    /** Tell the input method where the caret is, in screen coordinates. */
    void reportCursor() {
        int[] origin = new int[2];
        getLocationOnScreen(origin);
        Matrix matrix = new Matrix();
        matrix.setTranslate(origin[0], origin[1]);
        CursorAnchorInfo.Builder builder = new CursorAnchorInfo.Builder()
                .setMatrix(matrix)
                .setSelectionRange(0, 0)
                .setInsertionMarkerLocation(caretX, caretY, caretY + caretHeight * 0.8f,
                        caretY + caretHeight, CursorAnchorInfo.FLAG_HAS_VISIBLE_REGION);
        if (connection != null) {
            connection.describeComposition(builder);
        }
        if (Build.VERSION.SDK_INT >= 33) {
            builder.setEditorBoundsInfo(new android.view.inputmethod.EditorBoundsInfo.Builder()
                    .setEditorBounds(new android.graphics.RectF(caretX, caretY,
                            caretX + Math.max(caretWidth, 1), caretY + caretHeight))
                    .build());
        }
        imm.updateCursorAnchorInfo(this, builder.build());
    }

    InputMethodManager inputMethods() {
        return imm;
    }
}
