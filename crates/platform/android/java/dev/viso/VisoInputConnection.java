package dev.viso;

import android.text.Editable;
import android.text.Selection;
import android.view.KeyEvent;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.CursorAnchorInfo;
import android.view.inputmethod.InputConnection;

/**
 * The document an input method edits. The app owns the real text, so the
 * editable here holds only the composition in progress: empty between
 * compositions, the composing text while one runs. Every change becomes a
 * preedit or commit for the app, and the editable is emptied after each
 * commit.
 */
final class VisoInputConnection extends BaseInputConnection {
    private final VisoView view;
    private boolean composing;

    VisoInputConnection(VisoView view) {
        super(view, true);
        this.view = view;
    }

    @Override
    public boolean setComposingText(CharSequence text, int newCursorPosition) {
        super.setComposingText(text, newCursorPosition);
        report();
        return true;
    }

    @Override
    public boolean setComposingRegion(int start, int end) {
        super.setComposingRegion(start, end);
        report();
        return true;
    }

    @Override
    public boolean commitText(CharSequence text, int newCursorPosition) {
        endComposition();
        String s = text.toString();
        if (s.equals("\n")) {
            view.tapKey(KeyEvent.KEYCODE_ENTER);
        } else if (!s.isEmpty()) {
            VisoActivity.nativeCommit(s);
        }
        reset();
        return true;
    }

    @Override
    public boolean finishComposingText() {
        Editable e = getEditable();
        if (composing && e != null) {
            int start = getComposingSpanStart(e);
            int end = getComposingSpanEnd(e);
            String s = start >= 0 && end > start ? e.subSequence(start, end).toString() : "";
            endComposition();
            if (!s.isEmpty()) {
                VisoActivity.nativeCommit(s);
            }
        }
        super.finishComposingText();
        reset();
        return true;
    }

    @Override
    public boolean deleteSurroundingText(int before, int after) {
        if (composing) {
            super.deleteSurroundingText(before, after);
            report();
            return true;
        }
        for (int i = 0; i < before; i++) {
            view.tapKey(KeyEvent.KEYCODE_DEL);
        }
        for (int i = 0; i < after; i++) {
            view.tapKey(KeyEvent.KEYCODE_FORWARD_DEL);
        }
        return true;
    }

    @Override
    public boolean performEditorAction(int action) {
        view.tapKey(KeyEvent.KEYCODE_ENTER);
        return true;
    }

    @Override
    public boolean performContextMenuAction(int id) {
        switch (id) {
            case android.R.id.copy:
                VisoActivity.nativeEdit(0);
                return true;
            case android.R.id.cut:
                VisoActivity.nativeEdit(1);
                return true;
            case android.R.id.paste:
                VisoActivity.nativeEdit(2);
                return true;
            default:
                return false;
        }
    }

    @Override
    public boolean requestCursorUpdates(int mode) {
        boolean monitor = (mode & InputConnection.CURSOR_UPDATE_MONITOR) != 0;
        view.setCursorUpdates(monitor);
        if ((mode & InputConnection.CURSOR_UPDATE_IMMEDIATE) != 0) {
            view.reportCursor();
        }
        return true;
    }

    /** Drop the composition without committing it. */
    void cancelComposition() {
        endComposition();
        reset();
    }

    void describeComposition(CursorAnchorInfo.Builder builder) {
        Editable e = getEditable();
        if (composing && e != null) {
            int start = getComposingSpanStart(e);
            int end = getComposingSpanEnd(e);
            if (start >= 0 && end >= start) {
                builder.setComposingText(start, e.subSequence(start, end));
            }
        }
    }

    /** Report the composition as it stands to the app and the input method. */
    private void report() {
        Editable e = getEditable();
        if (e == null) {
            return;
        }
        int start = getComposingSpanStart(e);
        int end = getComposingSpanEnd(e);
        if (start < 0 || end <= start) {
            endComposition();
            return;
        }
        composing = true;
        int caret = Math.max(0, Math.min(Selection.getSelectionEnd(e), end) - start);
        VisoActivity.nativePreedit(e.subSequence(start, end).toString(), caret);
        int sel = Selection.getSelectionEnd(e);
        view.inputMethods().updateSelection(view, sel, sel, start, end);
    }

    private void endComposition() {
        if (composing) {
            composing = false;
            VisoActivity.nativePreedit("", 0);
        }
    }

    private void reset() {
        Editable e = getEditable();
        if (e != null) {
            removeComposingSpans(e);
            e.clear();
        }
        view.inputMethods().updateSelection(view, 0, 0, -1, -1);
    }
}
