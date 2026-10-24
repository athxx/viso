package dev.viso.media;

import android.content.Context;
import android.media.AudioAttributes;
import android.media.MediaDataSource;
import android.media.MediaPlayer;
import android.media.PlaybackParams;
import android.net.Uri;
import android.view.Surface;

import java.nio.ByteBuffer;

/**
 * One video's {@link MediaPlayer}, steered from the native loop thread. What
 * the player reports, on the thread it reports on, goes to the native side
 * through {@link #nativeEvent} until {@link #close} returns; after that the
 * native side frees what the events go to.
 */
final class VisoVideo implements MediaPlayer.OnPreparedListener,
        MediaPlayer.OnCompletionListener, MediaPlayer.OnErrorListener,
        MediaPlayer.OnInfoListener, MediaPlayer.OnSeekCompleteListener,
        MediaPlayer.OnVideoSizeChangedListener {
    /** Prepared: width, height, whether it has sound, duration in ms (-1 unknown). */
    static final int PREPARED = 0;
    static final int COMPLETED = 1;
    /** An error: what, extra. */
    static final int ERROR = 2;
    /** Buffering started (1) or ended (0). */
    static final int BUFFERING = 3;
    static final int SEEKED = 4;
    /** The picture's size changed: width, height. */
    static final int SIZE = 5;

    private final long mailbox;
    private final MediaPlayer player = new MediaPlayer();
    private boolean closed;
    private volatile boolean playing;
    private float rate = 1f;
    private float applied = 1f;

    /**
     * A player of {@code uri} ({@code file:}, {@code content:}, {@code http(s):},
     * an HLS playlist) or, when {@code bytes} is given, of those bytes, read in
     * place. Throws what {@link MediaPlayer#setDataSource} throws.
     */
    VisoVideo(long mailbox, Context context, String uri, ByteBuffer bytes) throws Exception {
        this.mailbox = mailbox;
        player.setOnPreparedListener(this);
        player.setOnCompletionListener(this);
        player.setOnErrorListener(this);
        player.setOnInfoListener(this);
        player.setOnSeekCompleteListener(this);
        player.setOnVideoSizeChangedListener(this);
        player.setAudioAttributes(new AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_MEDIA)
                .setContentType(AudioAttributes.CONTENT_TYPE_MOVIE)
                .build());
        if (bytes != null) {
            player.setDataSource(new Bytes(bytes));
        } else {
            player.setDataSource(context, Uri.parse(uri));
        }
        player.prepareAsync();
    }

    // Native side, once prepared.

    void setSurface(Surface surface) {
        player.setSurface(surface);
    }

    void play() {
        playing = true;
        player.start();
        if (applied != rate) {
            applyRate();
        }
    }

    void pause() {
        playing = false;
        player.pause();
    }

    /** To the frame at {@code ms}, not only the key frame before it. */
    void seek(long ms) {
        player.seekTo(ms, MediaPlayer.SEEK_CLOSEST);
    }

    /** The speed to play at, pitch kept; applied once it plays, as setting it starts it. */
    void setRate(float rate) {
        this.rate = rate;
        if (playing) {
            applyRate();
        }
    }

    private void applyRate() {
        try {
            PlaybackParams params = player.getPlaybackParams();
            player.setPlaybackParams(params.setSpeed(rate));
            applied = rate;
        } catch (IllegalArgumentException | IllegalStateException e) {
            // A speed the device cannot play at: it plays on at the last.
        }
    }

    int position() {
        return player.getCurrentPosition();
    }

    // Native side, at any time.

    void setVolume(float volume, boolean muted) {
        float level = muted ? 0f : volume;
        player.setVolume(level, level);
    }

    void setLooping(boolean looping) {
        player.setLooping(looping);
    }

    /** Releases the player; no event is reported once this returns. */
    void close() {
        synchronized (this) {
            closed = true;
        }
        player.release();
    }

    private void report(int kind, int a, int b, int c, long d) {
        synchronized (this) {
            if (!closed) {
                nativeEvent(mailbox, kind, a, b, c, d);
            }
        }
    }

    @Override
    public void onPrepared(MediaPlayer mp) {
        boolean audio = false;
        try {
            for (MediaPlayer.TrackInfo track : mp.getTrackInfo()) {
                if (track.getTrackType() == MediaPlayer.TrackInfo.MEDIA_TRACK_TYPE_AUDIO) {
                    audio = true;
                }
            }
        } catch (RuntimeException e) {
            audio = true;
        }
        report(PREPARED, mp.getVideoWidth(), mp.getVideoHeight(), audio ? 1 : 0, mp.getDuration());
    }

    @Override
    public void onCompletion(MediaPlayer mp) {
        playing = false;
        report(COMPLETED, 0, 0, 0, 0);
    }

    @Override
    public boolean onError(MediaPlayer mp, int what, int extra) {
        report(ERROR, what, extra, 0, 0);
        return true;
    }

    @Override
    public boolean onInfo(MediaPlayer mp, int what, int extra) {
        if (what == MediaPlayer.MEDIA_INFO_BUFFERING_START) {
            report(BUFFERING, 1, 0, 0, 0);
        } else if (what == MediaPlayer.MEDIA_INFO_BUFFERING_END) {
            report(BUFFERING, 0, 0, 0, 0);
        }
        return false;
    }

    @Override
    public void onSeekComplete(MediaPlayer mp) {
        report(SEEKED, 0, 0, 0, 0);
    }

    @Override
    public void onVideoSizeChanged(MediaPlayer mp, int width, int height) {
        report(SIZE, width, height, 0, 0);
    }

    private static native void nativeEvent(long mailbox, int kind, int a, int b, int c, long d);

    /** Bytes in native memory, read where they lie. */
    private static final class Bytes extends MediaDataSource {
        private final ByteBuffer bytes;

        Bytes(ByteBuffer bytes) {
            this.bytes = bytes;
        }

        @Override
        public int readAt(long position, byte[] buffer, int offset, int size) {
            int length = bytes.capacity();
            if (position >= length) {
                return -1;
            }
            int n = (int) Math.min(size, length - position);
            ByteBuffer view = bytes.duplicate();
            view.position((int) position);
            view.get(buffer, offset, n);
            return n;
        }

        @Override
        public long getSize() {
            return bytes.capacity();
        }

        @Override
        public void close() {
        }
    }
}
