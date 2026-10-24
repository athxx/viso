package dev.viso.media;

import android.Manifest;
import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.ImageFormat;
import android.hardware.camera2.CameraAccessException;
import android.hardware.camera2.CameraCaptureSession;
import android.hardware.camera2.CameraCharacteristics;
import android.hardware.camera2.CameraDevice;
import android.hardware.camera2.CameraManager;
import android.hardware.camera2.CaptureRequest;
import android.hardware.camera2.params.OutputConfiguration;
import android.hardware.camera2.params.SessionConfiguration;
import android.hardware.camera2.params.StreamConfigurationMap;
import android.hardware.display.DisplayManager;
import android.os.Build;
import android.os.Handler;
import android.os.HandlerThread;
import android.util.Range;
import android.util.Size;
import android.view.Display;
import android.view.Surface;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.concurrent.Executor;

import dev.viso.VisoActivity;

/**
 * One camera's capture into a native image reader's surface, steered from the
 * native loop thread. The camera opens on {@link #start} once the app may use
 * it, asking the user first where it has not been told, and closes on
 * {@link #stop}. What happens, on the camera's own thread, goes to the native
 * side through {@link #nativeEvent} until {@link #close} returns; after that
 * the native side frees what the events go to.
 */
final class VisoCamera extends CameraDevice.StateCallback
        implements DisplayManager.DisplayListener {
    /** It could not go on: why (DENIED, BUSY, GONE, FAILED). */
    static final int ERROR = 2;
    /** The display turned: its rotation in degrees. */
    static final int ROTATION = 6;

    static final int DENIED = 1;
    static final int BUSY = 2;
    static final int GONE = 3;
    static final int FAILED = 4;

    /** The first request code asked of the activity, clear of others'. */
    private static final int REQUEST = 0x5643;

    private final long mailbox;
    private final Context context;
    private final String id;
    private final Surface surface;
    private final int rate;
    private final HandlerThread thread = new HandlerThread("viso-camera");
    private final Handler handler;
    private final DisplayManager displays;

    // On the camera's thread.
    private boolean wanted;
    /** Opening, until the camera answers; the thread ends only after. */
    private boolean opening;
    /** Closed by the native side: the thread ends once nothing is open. */
    private boolean ending;
    private CameraDevice device;
    private CameraCaptureSession session;

    private boolean closed;

    /** A capture of camera {@code id} into {@code surface}, {@code rate} pictures a second. */
    VisoCamera(long mailbox, Context context, String id, Surface surface, int rate) {
        this.mailbox = mailbox;
        this.context = context;
        this.id = id;
        this.surface = surface;
        this.rate = rate;
        thread.start();
        handler = new Handler(thread.getLooper());
        displays = context.getSystemService(DisplayManager.class);
        displays.registerDisplayListener(this, handler);
        handler.post(this::reportRotation);
    }

    // Static, from any thread.

    /**
     * The cameras: each its facing (F, B or E), the clockwise degrees its
     * sensor lies turned in the device, a colon, then its id.
     */
    static String[] list(Context context) {
        CameraManager manager = context.getSystemService(CameraManager.class);
        List<String> out = new ArrayList<>();
        try {
            for (String id : manager.getCameraIdList()) {
                CameraCharacteristics c = manager.getCameraCharacteristics(id);
                Integer facing = c.get(CameraCharacteristics.LENS_FACING);
                Integer sensor = c.get(CameraCharacteristics.SENSOR_ORIENTATION);
                String side = facing == null ? "E"
                        : facing == CameraCharacteristics.LENS_FACING_FRONT ? "F"
                        : facing == CameraCharacteristics.LENS_FACING_BACK ? "B" : "E";
                out.add(side + (sensor == null ? 0 : sensor) + ":" + id);
            }
        } catch (CameraAccessException | RuntimeException e) {
            // No camera service, or a camera gone while listed.
        }
        return out.toArray(new String[0]);
    }

    /**
     * The ways camera {@code id} captures into an image reader: for each size,
     * each rate it keeps up at there, as width, height, rate.
     */
    static int[] modes(Context context, String id) {
        CameraManager manager = context.getSystemService(CameraManager.class);
        List<Integer> out = new ArrayList<>();
        try {
            CameraCharacteristics c = manager.getCameraCharacteristics(id);
            StreamConfigurationMap map = c.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP);
            Range<Integer>[] ranges = c.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES);
            if (map == null) {
                return new int[0];
            }
            List<Integer> rates = new ArrayList<>();
            if (ranges != null) {
                for (Range<Integer> range : ranges) {
                    if (!rates.contains(range.getUpper())) {
                        rates.add(range.getUpper());
                    }
                }
            }
            if (rates.isEmpty()) {
                rates.add(30);
            }
            Size[] sizes = map.getOutputSizes(ImageFormat.YUV_420_888);
            for (Size size : sizes == null ? new Size[0] : sizes) {
                long least = map.getOutputMinFrameDuration(ImageFormat.YUV_420_888, size);
                long most = least > 0 ? Math.round(1e9 / least) : Integer.MAX_VALUE;
                for (int r : rates) {
                    if (r <= most) {
                        out.add(size.getWidth());
                        out.add(size.getHeight());
                        out.add(r);
                    }
                }
            }
        } catch (CameraAccessException | RuntimeException e) {
            return new int[0];
        }
        int[] modes = new int[out.size()];
        for (int i = 0; i < modes.length; i++) {
            modes[i] = out.get(i);
        }
        return modes;
    }

    // Native side.

    /** Opens the camera, asking {@code activity} for leave first where it has none. */
    void start(Activity activity) {
        handler.post(() -> {
            wanted = true;
            if (device != null || opening) {
                return;
            }
            if (context.checkSelfPermission(Manifest.permission.CAMERA)
                    == PackageManager.PERMISSION_GRANTED) {
                open();
            } else if (activity != null) {
                ask(activity);
            } else {
                report(ERROR, DENIED);
            }
        });
    }

    /** Closes the camera; the light goes out. */
    void stop() {
        handler.post(() -> {
            wanted = false;
            shut();
        });
    }

    /** Closes the camera and its thread; no event is reported once this returns. */
    void close() {
        synchronized (this) {
            closed = true;
        }
        displays.unregisterDisplayListener(this);
        handler.post(() -> {
            ending = true;
            wanted = false;
            shut();
            end();
        });
    }

    /** Ends the thread once the native side closed and no camera is opening. */
    private void end() {
        if (ending && !opening) {
            thread.quitSafely();
        }
    }

    // The camera's thread.

    private void open() {
        try {
            context.getSystemService(CameraManager.class).openCamera(id, this, handler);
            opening = true;
        } catch (SecurityException e) {
            report(ERROR, DENIED);
        } catch (CameraAccessException e) {
            report(ERROR, e.getReason() == CameraAccessException.CAMERA_DISABLED ? DENIED
                    : e.getReason() == CameraAccessException.CAMERA_IN_USE
                            || e.getReason() == CameraAccessException.MAX_CAMERAS_IN_USE ? BUSY
                    : e.getReason() == CameraAccessException.CAMERA_DISCONNECTED ? GONE : FAILED);
        } catch (RuntimeException e) {
            report(ERROR, FAILED);
        }
    }

    private void shut() {
        if (session != null) {
            session.close();
            session = null;
        }
        if (device != null) {
            device.close();
            device = null;
        }
    }

    @Override
    public void onOpened(CameraDevice opened) {
        opening = false;
        if (!wanted) {
            opened.close();
            end();
            return;
        }
        device = opened;
        CameraCaptureSession.StateCallback configured = new CameraCaptureSession.StateCallback() {
            @Override
            public void onConfigured(CameraCaptureSession made) {
                if (device != opened) {
                    made.close();
                    return;
                }
                session = made;
                repeat(opened, made);
            }

            @Override
            public void onConfigureFailed(CameraCaptureSession made) {
                report(ERROR, FAILED);
            }
        };
        try {
            if (Build.VERSION.SDK_INT >= 28) {
                Executor executor = handler::post;
                opened.createCaptureSession(new SessionConfiguration(
                        SessionConfiguration.SESSION_REGULAR,
                        Collections.singletonList(new OutputConfiguration(surface)),
                        executor, configured));
            } else {
                legacySession(opened, configured);
            }
        } catch (CameraAccessException | RuntimeException e) {
            report(ERROR, FAILED);
        }
    }

    @SuppressWarnings("deprecation")
    private void legacySession(CameraDevice opened, CameraCaptureSession.StateCallback configured)
            throws CameraAccessException {
        opened.createCaptureSession(Collections.singletonList(surface), configured, handler);
    }

    /** Captures into the surface, again and again, at the rate asked. */
    private void repeat(CameraDevice opened, CameraCaptureSession made) {
        try {
            CaptureRequest.Builder request = opened.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW);
            request.addTarget(surface);
            Range<Integer> range = range(opened.getId());
            if (range != null) {
                request.set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, range);
            }
            made.setRepeatingRequest(request.build(), null, handler);
        } catch (CameraAccessException | RuntimeException e) {
            report(ERROR, FAILED);
        }
    }

    /**
     * The exposure's frame rate range reaching the rate asked, as low as it
     * goes below it: the camera slows in the dark rather than go dim.
     */
    private Range<Integer> range(String camera) throws CameraAccessException {
        CameraCharacteristics c = context.getSystemService(CameraManager.class)
                .getCameraCharacteristics(camera);
        Range<Integer>[] ranges = c.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES);
        Range<Integer> best = null;
        for (Range<Integer> r : ranges == null ? new Range[0] : ranges) {
            if (r.getUpper() == rate && (best == null || r.getLower() < best.getLower())) {
                best = r;
            }
        }
        return best;
    }

    @Override
    public void onDisconnected(CameraDevice gone) {
        opening = false;
        gone.close();
        end();
        if (device == gone) {
            device = null;
            session = null;
            report(ERROR, GONE);
        }
    }

    @Override
    public void onError(CameraDevice failed, int error) {
        opening = false;
        failed.close();
        end();
        if (device == failed) {
            device = null;
            session = null;
        }
        report(ERROR, error == ERROR_CAMERA_IN_USE || error == ERROR_MAX_CAMERAS_IN_USE ? BUSY
                : error == ERROR_CAMERA_DISABLED ? DENIED : FAILED);
    }

    // The display, on the camera's thread.

    @Override
    public void onDisplayAdded(int display) {}

    @Override
    public void onDisplayRemoved(int display) {}

    @Override
    public void onDisplayChanged(int display) {
        if (display == Display.DEFAULT_DISPLAY) {
            reportRotation();
        }
    }

    private void reportRotation() {
        Display display = displays.getDisplay(Display.DEFAULT_DISPLAY);
        if (display != null) {
            report(ROTATION, display.getRotation() * 90);
        }
    }

    // Leave, asked of the activity on its thread.

    private void ask(Activity activity) {
        VisoActivity.addResults(RESULTS);
        activity.runOnUiThread(() -> {
            synchronized (ASKING) {
                ASKING.add(this);
            }
            activity.requestPermissions(new String[] {Manifest.permission.CAMERA}, REQUEST);
        });
    }

    private static final List<VisoCamera> ASKING = new ArrayList<>();

    private static final VisoActivity.Results RESULTS = new VisoActivity.Results() {
        @Override
        public void onActivityResult(Activity activity, int request, int result, Intent data) {}

        @Override
        public void onRequestPermissionsResult(Activity activity, int request,
                String[] permissions, int[] grants) {
            if (request != REQUEST) {
                return;
            }
            boolean granted = grants.length > 0 && grants[0] == PackageManager.PERMISSION_GRANTED;
            List<VisoCamera> asked;
            synchronized (ASKING) {
                asked = new ArrayList<>(ASKING);
                ASKING.clear();
            }
            for (VisoCamera camera : asked) {
                camera.handler.post(() -> {
                    if (!granted) {
                        camera.report(ERROR, DENIED);
                    } else if (camera.wanted && camera.device == null && !camera.opening) {
                        camera.open();
                    }
                });
            }
        }
    };

    private void report(int kind, int a) {
        synchronized (this) {
            if (!closed) {
                nativeEvent(mailbox, kind, a, 0, 0, 0);
            }
        }
    }

    private static native void nativeEvent(long mailbox, int kind, int a, int b, int c, long d);
}
