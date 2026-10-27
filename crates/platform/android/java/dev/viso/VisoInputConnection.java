package dev.viso;

import android.text.Editable;
import android.text.Selection;
import android.text.style.BackgroundColorSpan;
import android.text.style.UnderlineSpan;
import android.view.KeyEvent;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.CursorAnchorInfo;
import android.view.inputmethod.InputConnection;

/**
 * The document an input method edits. The app owns the real text and shows
 * the input method a stretch of it around the caret; the editable here
 * mirrors that stretch, the composition included, so the input method reads
 * what lies before and after the caret at once. Each edit is applied to the
 * mirror as the app will apply it and sent to the app as a preedit, a
 * commit or an edit of a range, its offsets bytes of the field's text.
 * When the app shows no text (a secret, a control that keeps its own
 * document) the editable holds only the composition and is emptied after
 * each commit.
 */
final class VisoInputConnection extends BaseInputConnection {
    private static final int[] NO_CLAUSES = new int[0];
    private final VisoView view;
    private boolean composing;
    /** Whether the editable mirrors text the app shows. */
    private boolean shown;
    /** Where the editable starts in the field's text, bytes. */
    private long offset;

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
        Editable e = getEditable();
        if (!shown || e == null) {
            super.setComposingRegion(start, end);
            report();
            return true;
        }
        int a = clamp(Math.min(start, end), e), b = clamp(Math.max(start, end), e);
        if (a == b) {
            return finishComposingText();
        }
        // A typed word taken up again: it becomes the composition.
        String s = e.subSequence(a, b).toString();
        int caret = clamp(Selection.getSelectionEnd(e), e);
        caret = Math.max(a, Math.min(caret, b));
        composing = true;
        VisoActivity.nativeImeEdit(1, bytesAt(e, a), bytesAt(e, b), s, utf8(s, 0, caret - a));
        super.setComposingRegion(a, b);
        updateSelection();
        return true;
    }

    @Override
    public boolean commitText(CharSequence text, int newCursorPosition) {
        String s = text.toString();
        if (s.equals("\n")) {
            finishComposingText();
            view.tapKey(KeyEvent.KEYCODE_ENTER);
            return true;
        }
        if (!shown) {
            endComposition();
            if (!s.isEmpty()) {
                VisoActivity.nativeCommit(s);
            }
            reset();
            return true;
        }
        // Over the composition, else over the selection.
        VisoActivity.nativeCommit(s);
        composing = false;
        super.commitText(text, 1);
        updateSelection();
        return true;
    }

    @Override
    public boolean finishComposingText() {
        Editable e = getEditable();
        if (composing && e != null) {
            int start = getComposingSpanStart(e);
            int end = getComposingSpanEnd(e);
            String s = start >= 0 && end > start ? e.subSequence(start, end).toString() : "";
            composing = false;
            if (s.isEmpty()) {
                VisoActivity.nativePreedit("", 0, NO_CLAUSES, -1, -1);
            } else {
                VisoActivity.nativeCommit(s);
            }
        }
        super.finishComposingText();
        if (shown) {
            updateSelection();
        } else {
            reset();
        }
        return true;
    }

    @Override
    public boolean deleteSurroundingText(int before, int after) {
        Editable e = getEditable();
        if (!shown || e == null) {
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
        // Around the selection and the composition, as the editable deletes.
        int a = Math.min(Selection.getSelectionStart(e), Selection.getSelectionEnd(e));
        int b = Math.max(Selection.getSelectionStart(e), Selection.getSelectionEnd(e));
        int cs = getComposingSpanStart(e), ce = getComposingSpanEnd(e);
        if (cs >= 0 && ce >= 0) {
            a = Math.min(a, Math.min(cs, ce));
            b = Math.max(b, Math.max(cs, ce));
        }
        a = clamp(a, e);
        b = clamp(b, e);
        int stop = Math.min(e.length(), b + Math.max(0, after));
        int from = Math.max(0, a - Math.max(0, before));
        if (stop > b) {
            VisoActivity.nativeImeEdit(0, bytesAt(e, b), bytesAt(e, stop), "", 0);
        }
        if (from < a) {
            VisoActivity.nativeImeEdit(0, bytesAt(e, from), bytesAt(e, a), "", 0);
        }
        super.deleteSurroundingText(before, after);
        updateSelection();
        return true;
    }

    @Override
    public boolean deleteSurroundingTextInCodePoints(int before, int after) {
        Editable e = getEditable();
        if (e == null) {
            return false;
        }
        int a = Math.min(Selection.getSelectionStart(e), Selection.getSelectionEnd(e));
        int b = Math.max(Selection.getSelectionStart(e), Selection.getSelectionEnd(e));
        int units = 0;
        for (int i = 0, at = a; i < before && at > 0; i++) {
            int step = Character.isLowSurrogate(e.charAt(at - 1)) && at > 1
                    && Character.isHighSurrogate(e.charAt(at - 2)) ? 2 : 1;
            at -= step;
            units += step;
        }
        int unitsAfter = 0;
        for (int i = 0, at = b; i < after && at < e.length(); i++) {
            int step = Character.isHighSurrogate(e.charAt(at)) && at + 1 < e.length()
                    && Character.isLowSurrogate(e.charAt(at + 1)) ? 2 : 1;
            at += step;
            unitsAfter += step;
        }
        return deleteSurroundingText(units, unitsAfter);
    }

    @Override
    public boolean setSelection(int start, int end) {
        Editable e = getEditable();
        if (shown && e != null) {
            int a = clamp(start, e), b = clamp(end, e);
            VisoActivity.nativeImeEdit(2, bytesAt(e, a), bytesAt(e, b), null, 0);
        }
        super.setSelection(start, end);
        updateSelection();
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
            case android.R.id.selectAll:
                VisoActivity.nativeEdit(3);
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

    /**
     * Take the text the app shows: {@code text} from byte {@code from} of the
     * field's text, its selection and composition UTF-16 offsets into it
     * (the composition -1 when there is none). A composition under way stays,
     * over the app's own or else over its selection. The input method hears
     * only of a change it did not make itself.
     */
    void show(boolean isShown, String text, long from, int anchor, int focus, int composeStart,
            int composeEnd) {
        Editable e = getEditable();
        if (e == null) {
            return;
        }
        String was = e.toString();
        int wasStart = Selection.getSelectionStart(e), wasEnd = Selection.getSelectionEnd(e);
        int wasCs = getComposingSpanStart(e), wasCe = getComposingSpanEnd(e);
        String marked = null;
        int markedCaret = 0;
        if (composing && wasCs >= 0 && wasCe > wasCs) {
            marked = was.substring(wasCs, wasCe);
            markedCaret = Math.max(0, Math.min(wasEnd - wasCs, marked.length()));
        }
        shown = isShown;
        offset = from;
        removeComposingSpans(e);
        if (isShown) {
            e.replace(0, e.length(), text);
            int a = clamp(anchor, e), f = clamp(focus, e);
            Selection.setSelection(e, a, f);
            if (marked != null) {
                int ms, me;
                if (composeStart >= 0 && composeEnd >= composeStart) {
                    ms = clamp(composeStart, e);
                    me = clamp(composeEnd, e);
                } else {
                    ms = Math.min(a, f);
                    me = Math.max(a, f);
                }
                e.replace(ms, me, marked);
                super.setComposingRegion(ms, ms + marked.length());
                Selection.setSelection(e, ms + markedCaret);
            }
        } else if (marked != null) {
            e.replace(0, e.length(), marked);
            super.setComposingRegion(0, marked.length());
            Selection.setSelection(e, markedCaret);
        } else {
            e.clear();
        }
        if (!e.toString().equals(was) || Selection.getSelectionStart(e) != wasStart
                || Selection.getSelectionEnd(e) != wasEnd || getComposingSpanStart(e) != wasCs
                || getComposingSpanEnd(e) != wasCe) {
            updateSelection();
        }
    }

    /** Drop the composition without committing it. */
    void cancelComposition() {
        endComposition();
        shown = false;
        reset();
    }

    void describeComposition(CursorAnchorInfo.Builder builder) {
        Editable e = getEditable();
        if (e == null) {
            return;
        }
        builder.setSelectionRange(Selection.getSelectionStart(e), Selection.getSelectionEnd(e));
        if (composing) {
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
        // The input method underlines each clause and shades the one it
        // is converting.
        UnderlineSpan[] lines = e.getSpans(start, end, UnderlineSpan.class);
        int[] clauses = new int[lines.length * 2];
        for (int i = 0; i < lines.length; i++) {
            clauses[2 * i] = Math.max(e.getSpanStart(lines[i]), start) - start;
            clauses[2 * i + 1] = Math.min(e.getSpanEnd(lines[i]), end) - start;
        }
        int selectedStart = -1;
        int selectedEnd = -1;
        for (BackgroundColorSpan shade : e.getSpans(start, end, BackgroundColorSpan.class)) {
            int from = Math.max(e.getSpanStart(shade), start);
            int to = Math.min(e.getSpanEnd(shade), end);
            if (from < to) {
                selectedStart = from - start;
                selectedEnd = to - start;
                break;
            }
        }
        VisoActivity.nativePreedit(
                e.subSequence(start, end).toString(), caret, clauses, selectedStart, selectedEnd);
        updateSelection();
    }

    private void endComposition() {
        if (composing) {
            composing = false;
            VisoActivity.nativePreedit("", 0, NO_CLAUSES, -1, -1);
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

    /** Tell the input method the selection and the composition as they stand. */
    private void updateSelection() {
        Editable e = getEditable();
        if (e == null) {
            return;
        }
        view.inputMethods().updateSelection(view, Selection.getSelectionStart(e),
                Selection.getSelectionEnd(e), getComposingSpanStart(e), getComposingSpanEnd(e));
    }

    /** UTF-16 offset {@code at} of the editable as a byte of the field's text. */
    private long bytesAt(Editable e, int at) {
        return offset + utf8(e, 0, at);
    }

    /** The UTF-8 length of {@code s} from {@code from} to {@code to}. */
    private static int utf8(CharSequence s, int from, int to) {
        int n = 0;
        for (int i = from; i < to; i++) {
            char c = s.charAt(i);
            if (c < 0x80) {
                n += 1;
            } else if (c < 0x800) {
                n += 2;
            } else if (Character.isHighSurrogate(c) && i + 1 < to
                    && Character.isLowSurrogate(s.charAt(i + 1))) {
                n += 4;
                i++;
            } else {
                n += 3;
            }
        }
        return n;
    }

    /** {@code at} within the editable and off a surrogate pair's second half. */
    private static int clamp(int at, Editable e) {
        int c = Math.max(0, Math.min(at, e.length()));
        if (c > 0 && c < e.length() && Character.isLowSurrogate(e.charAt(c))
                && Character.isHighSurrogate(e.charAt(c - 1))) {
            c--;
        }
        return c;
    }
}
