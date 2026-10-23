package dev.viso;

import android.app.Activity;
import android.content.ClipData;
import android.content.ClipDescription;
import android.content.ClipboardManager;
import android.content.ContentResolver;
import android.content.Context;
import android.database.Cursor;
import android.net.Uri;
import android.provider.OpenableColumns;
import android.view.DragAndDropPermissions;
import android.view.DragEvent;
import android.graphics.Matrix;
import android.graphics.Rect;
import android.os.Build;
import android.view.ActionMode;
import android.view.InputDevice;
import android.view.KeyCharacterMap;
import android.view.KeyEvent;
import android.view.Menu;
import android.view.MenuItem;
import android.view.MotionEvent;
import android.view.SurfaceView;
import android.view.View;
import android.view.ViewConfiguration;
import android.view.inputmethod.CursorAnchorInfo;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;
import android.widget.Magnifier;
import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;

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
    /** The focused field takes a secret. */
    private boolean secure;
    /** What the focused field holds, one of the {@code KIND_} constants. */
    private int kind;
    /** The focused field holds lines. */
    private boolean multiline;

    static final int KIND_TEXT = 0;
    static final int KIND_DIGITS = 1;
    static final int KIND_INTEGER = 2;
    static final int KIND_DECIMAL = 3;
    static final int KIND_PHONE = 4;
    static final int KIND_EMAIL = 5;
    static final int KIND_URL = 6;
    static final int KIND_SEARCH = 7;
    private float caretX, caretY, caretWidth, caretHeight;
    private boolean cursorUpdates;
    private int deadKey;

    /** The items of the edit menu, the {@code MENU_} bits. */
    static final int MENU_CUT = 1;
    static final int MENU_COPY = 2;
    static final int MENU_PASTE = 4;
    static final int MENU_SELECT_ALL = 8;
    /** The {@code nativeEdit} actions of the edit menu. */
    private static final int EDIT_COPY = 0;
    private static final int EDIT_CUT = 1;
    private static final int EDIT_PASTE = 2;
    private static final int EDIT_SELECT_ALL = 3;
    private static final int EDIT_MENU_CLOSED = 4;
    /** The floating edit menu while it shows. */
    private ActionMode editMenu;
    /** The text magnifier, made the first time it shows (API 28 and later). */
    private Magnifier magnifier;
    private int menuItems;
    /** What the edit menu points at and keeps clear of, in pixels of the view. */
    private final Rect menuAt = new Rect();
    /** The edit menu is closing at the app's word, not the user's. */
    private boolean menuClosing;

    /** The {@code nativeDrag} steps of a drag from another app. */
    private static final int DRAG_OVER = 0;
    private static final int DRAG_LEAVE = 1;
    /** The kinds a drag offers, as {@code nativeDrag} takes them. */
    private static final int DRAG_TEXT = 1;
    private static final int DRAG_URI = 2;
    private static final int DRAG_FILES = 4;
    /** A drag from another app is over the view. */
    private boolean dragInside;
    /** Drops so far, naming each one's folder of copied files. */
    private int drops;

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

    // Drags from other apps.

    @Override
    public boolean onDragEvent(DragEvent event) {
        switch (event.getAction()) {
            case DragEvent.ACTION_DRAG_STARTED:
                return true;
            case DragEvent.ACTION_DRAG_ENTERED:
            case DragEvent.ACTION_DRAG_LOCATION:
                dragInside = true;
                VisoActivity.nativeDrag(DRAG_OVER, event.getX(), event.getY(),
                        dragKinds(event.getClipDescription()), false);
                return true;
            case DragEvent.ACTION_DRAG_EXITED:
            case DragEvent.ACTION_DRAG_ENDED:
                if (dragInside) {
                    dragInside = false;
                    VisoActivity.nativeDrag(DRAG_LEAVE, 0, 0, 0, false);
                }
                return true;
            case DragEvent.ACTION_DROP:
                dragInside = false;
                return drop(event);
            default:
                return false;
        }
    }

    /** The kinds a drag described by {@code description} offers. */
    private static int dragKinds(ClipDescription description) {
        if (description == null) {
            return 0;
        }
        int kinds = 0;
        for (int i = 0; i < description.getMimeTypeCount(); i++) {
            String type = description.getMimeType(i);
            if (type.equals(ClipDescription.MIMETYPE_TEXT_URILIST)) {
                kinds |= DRAG_URI | DRAG_FILES;
            } else if (type.startsWith("text/")) {
                kinds |= DRAG_TEXT;
            } else {
                kinds |= DRAG_FILES;
            }
        }
        return kinds;
    }

    /**
     * The drag was let go over the view: the app is asked at once whether
     * it takes it there, and if so its text, link and files go to it, the
     * files another app shares by content copied into the cache first.
     */
    private boolean drop(DragEvent event) {
        float x = event.getX();
        float y = event.getY();
        int kinds = dragKinds(event.getClipDescription());
        if (VisoActivity.nativeDrag(DRAG_OVER, x, y, kinds, true) == 0) {
            VisoActivity.nativeDrag(DRAG_LEAVE, 0, 0, 0, false);
            return false;
        }
        ClipData clip = event.getClipData();
        String text = null;
        String link = null;
        ArrayList<String> files = new ArrayList<>();
        ArrayList<Uri> shared = new ArrayList<>();
        for (int i = 0; clip != null && i < clip.getItemCount(); i++) {
            ClipData.Item item = clip.getItemAt(i);
            if (text == null && item.getText() != null) {
                text = item.getText().toString();
            }
            Uri uri = item.getUri();
            if (uri == null) {
                continue;
            }
            String scheme = uri.getScheme();
            if (ContentResolver.SCHEME_FILE.equals(scheme) && uri.getPath() != null) {
                files.add(uri.getPath());
            } else if (ContentResolver.SCHEME_CONTENT.equals(scheme)) {
                shared.add(uri);
            } else if (link == null) {
                link = uri.toString();
            }
        }
        if (shared.isEmpty()) {
            VisoActivity.nativeDrop(x, y, text, link, files.toArray(new String[0]));
            return true;
        }
        Context context = getContext();
        DragAndDropPermissions permissions = context instanceof Activity
                ? ((Activity) context).requestDragAndDropPermissions(event)
                : null;
        File folder = new File(context.getCacheDir(), "viso-drop-" + (++drops));
        String dropText = text;
        String dropLink = link;
        new Thread(() -> {
            folder.mkdirs();
            ContentResolver resolver = context.getContentResolver();
            for (int i = 0; i < shared.size(); i++) {
                File copy = copyOut(resolver, shared.get(i), folder, i);
                if (copy != null) {
                    files.add(copy.getPath());
                }
            }
            if (permissions != null) {
                permissions.release();
            }
            VisoActivity.nativeDrop(x, y, dropText, dropLink, files.toArray(new String[0]));
        }, "viso-drop").start();
        return true;
    }

    /** A copy in {@code folder} of what {@code uri} holds, named as its provider names it. */
    private static File copyOut(ContentResolver resolver, Uri uri, File folder, int index) {
        String name = null;
        try (Cursor cursor = resolver.query(uri,
                new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) {
                name = cursor.getString(0);
            }
        } catch (RuntimeException e) {
            // The provider names nothing: the copy is numbered.
        }
        if (name == null || name.isEmpty() || name.contains("/")) {
            name = "item-" + index;
        }
        File copy = new File(folder, name);
        try (InputStream in = resolver.openInputStream(uri);
                OutputStream out = new FileOutputStream(copy)) {
            if (in == null) {
                return null;
            }
            byte[] buffer = new byte[64 * 1024];
            for (int n; (n = in.read(buffer)) > 0; ) {
                out.write(buffer, 0, n);
            }
            return copy;
        } catch (Exception e) {
            copy.delete();
            return null;
        }
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
        info.inputType = inputType();
        // A single-line field's return key finishes the entry; the action
        // reaches the app as Enter.
        int action = multiline ? EditorInfo.IME_ACTION_NONE
                : kind == KIND_SEARCH ? EditorInfo.IME_ACTION_SEARCH
                : EditorInfo.IME_ACTION_DONE;
        info.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN | EditorInfo.IME_FLAG_NO_EXTRACT_UI
                | action;
        if (multiline) {
            info.imeOptions |= EditorInfo.IME_FLAG_NO_ENTER_ACTION;
        }
        if (secure) {
            info.imeOptions |= EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING;
        }
        info.initialSelStart = 0;
        info.initialSelEnd = 0;
        connection = new VisoInputConnection(this);
        return connection;
    }

    /** The keyboard the focused field asks for. */
    private int inputType() {
        // A secure field's keyboard composes, suggests and learns nothing.
        if (secure) {
            return kind == KIND_DIGITS
                    ? EditorInfo.TYPE_CLASS_NUMBER | EditorInfo.TYPE_NUMBER_VARIATION_PASSWORD
                    : EditorInfo.TYPE_CLASS_TEXT | EditorInfo.TYPE_TEXT_VARIATION_PASSWORD;
        }
        switch (kind) {
            case KIND_DIGITS:
                return EditorInfo.TYPE_CLASS_NUMBER;
            case KIND_INTEGER:
                return EditorInfo.TYPE_CLASS_NUMBER | EditorInfo.TYPE_NUMBER_FLAG_SIGNED;
            case KIND_DECIMAL:
                return EditorInfo.TYPE_CLASS_NUMBER | EditorInfo.TYPE_NUMBER_FLAG_SIGNED
                        | EditorInfo.TYPE_NUMBER_FLAG_DECIMAL;
            case KIND_PHONE:
                return EditorInfo.TYPE_CLASS_PHONE;
            case KIND_EMAIL:
                return EditorInfo.TYPE_CLASS_TEXT
                        | EditorInfo.TYPE_TEXT_VARIATION_EMAIL_ADDRESS;
            case KIND_URL:
                return EditorInfo.TYPE_CLASS_TEXT | EditorInfo.TYPE_TEXT_VARIATION_URI;
            default:
                return multiline
                        ? EditorInfo.TYPE_CLASS_TEXT | EditorInfo.TYPE_TEXT_FLAG_MULTI_LINE
                        : EditorInfo.TYPE_CLASS_TEXT;
        }
    }

    void setTextInput(boolean enabled, boolean secret, int kindOf, boolean lines, float x,
            float y, float width, float height) {
        caretX = x;
        caretY = y;
        caretWidth = width;
        caretHeight = height;
        if (enabled != textInput || secret != secure || kindOf != kind || lines != multiline) {
            textInput = enabled;
            secure = secret;
            kind = kindOf;
            multiline = lines;
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

    /**
     * Show the magnifier over the content around {@code x, y}, move it there,
     * or put it away. Nothing shows before API 28.
     */
    @SuppressWarnings("deprecation")
    void setLoupe(boolean show, float x, float y) {
        if (Build.VERSION.SDK_INT < 28) {
            return;
        }
        if (!show) {
            if (magnifier != null) {
                magnifier.dismiss();
            }
            return;
        }
        if (magnifier == null) {
            magnifier = Build.VERSION.SDK_INT >= 29
                    ? new Magnifier.Builder(this).build()
                    : new Magnifier(this);
        }
        magnifier.show(x, y);
    }

    /**
     * Show the floating edit menu beside {@code x, y, width, height} with the
     * {@code MENU_} {@code items}, move it or change its items while it shows,
     * or close it.
     */
    void setEditMenu(boolean show, float x, float y, float width, float height, int items) {
        if (!show) {
            if (editMenu != null) {
                menuClosing = true;
                editMenu.finish();
                menuClosing = false;
                editMenu = null;
            }
            return;
        }
        menuAt.set((int) Math.floor(x), (int) Math.floor(y), (int) Math.ceil(x + width),
                (int) Math.ceil(y + height));
        if (editMenu != null) {
            if (items != menuItems) {
                menuItems = items;
                editMenu.invalidate();
            }
            editMenu.invalidateContentRect();
            return;
        }
        menuItems = items;
        editMenu = startActionMode(new ActionMode.Callback2() {
            @Override
            public boolean onCreateActionMode(ActionMode mode, Menu menu) {
                menu.add(Menu.NONE, android.R.id.cut, 0, android.R.string.cut);
                menu.add(Menu.NONE, android.R.id.copy, 1, android.R.string.copy);
                menu.add(Menu.NONE, android.R.id.paste, 2, android.R.string.paste);
                menu.add(Menu.NONE, android.R.id.selectAll, 3, android.R.string.selectAll);
                return true;
            }

            @Override
            public boolean onPrepareActionMode(ActionMode mode, Menu menu) {
                ClipboardManager clipboard =
                        (ClipboardManager) getContext().getSystemService(Context.CLIPBOARD_SERVICE);
                boolean clip = clipboard != null && clipboard.hasPrimaryClip();
                menu.findItem(android.R.id.cut).setVisible((menuItems & MENU_CUT) != 0);
                menu.findItem(android.R.id.copy).setVisible((menuItems & MENU_COPY) != 0);
                menu.findItem(android.R.id.paste).setVisible((menuItems & MENU_PASTE) != 0 && clip);
                menu.findItem(android.R.id.selectAll)
                        .setVisible((menuItems & MENU_SELECT_ALL) != 0);
                return true;
            }

            @Override
            public boolean onActionItemClicked(ActionMode mode, MenuItem item) {
                int id = item.getItemId();
                if (id == android.R.id.cut) {
                    VisoActivity.nativeEdit(EDIT_CUT);
                } else if (id == android.R.id.copy) {
                    VisoActivity.nativeEdit(EDIT_COPY);
                } else if (id == android.R.id.paste) {
                    VisoActivity.nativeEdit(EDIT_PASTE);
                } else if (id == android.R.id.selectAll) {
                    VisoActivity.nativeEdit(EDIT_SELECT_ALL);
                } else {
                    return false;
                }
                mode.finish();
                return true;
            }

            @Override
            public void onDestroyActionMode(ActionMode mode) {
                if (editMenu == mode) {
                    editMenu = null;
                }
                if (!menuClosing) {
                    VisoActivity.nativeEdit(EDIT_MENU_CLOSED);
                }
            }

            @Override
            public void onGetContentRect(ActionMode mode, View view, Rect outRect) {
                outRect.set(menuAt);
            }
        }, ActionMode.TYPE_FLOATING);
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
