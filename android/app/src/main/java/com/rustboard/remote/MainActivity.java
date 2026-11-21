package com.rustboard.remote;

import android.app.Activity;
import android.Manifest;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.os.Build;
import android.os.Bundle;
import android.os.VibrationEffect;
import android.os.Vibrator;
import android.view.View;
import android.view.WindowManager;
import android.webkit.JavascriptInterface;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;

import com.google.zxing.integration.android.IntentIntegrator;
import com.google.zxing.integration.android.IntentResult;

import org.json.JSONObject;

import java.io.File;
import java.io.FileOutputStream;
import java.io.PrintWriter;
import java.io.StringWriter;
import java.nio.charset.StandardCharsets;

/**
 * Rustboard Remote: a full-screen WebView that shows the remote (assets/index.html).
 * The page talks to the Rustboard app on the PC over Wi-Fi. Landscape only (see the manifest).
 */
public class MainActivity extends Activity {
    private static final int REQ_CAMERA = 71;
    private WebView web;

    /** Where a crash is written, so the next start can show it (no PC cable needed to find the cause). */
    private static File crashFile(Context c) {
        return new File(c.getFilesDir(), "last-crash.txt");
    }

    private void keepCrashes() {
        final Context app = getApplicationContext();
        final Thread.UncaughtExceptionHandler old = Thread.getDefaultUncaughtExceptionHandler();
        Thread.setDefaultUncaughtExceptionHandler((t, e) -> {
            try {
                StringWriter sw = new StringWriter();
                e.printStackTrace(new PrintWriter(sw));
                try (FileOutputStream out = new FileOutputStream(crashFile(app))) {
                    out.write(sw.toString().getBytes(StandardCharsets.UTF_8));
                }
            } catch (Throwable ignored) {
                // never hide the real crash
            }
            if (old != null) old.uncaughtException(t, e);
        });
    }

    private String readCrash() {
        try {
            File f = crashFile(this);
            if (!f.isFile()) return "";
            // (java.nio.file needs Android 8, this app supports Android 7)
            try (java.io.FileInputStream in = new java.io.FileInputStream(f)) {
                java.io.ByteArrayOutputStream bo = new java.io.ByteArrayOutputStream();
                byte[] buf = new byte[4096];
                int n;
                while ((n = in.read(buf)) > 0) bo.write(buf, 0, n);
                return new String(bo.toByteArray(), StandardCharsets.UTF_8);
            }
        } catch (Throwable t) {
            return "";
        }
    }

    private void js(String code) {
        web.evaluateJavascript(code, null);
    }

    private void scanError(String msg) {
        js("window.onQrError && window.onQrError(" + JSONObject.quote(msg) + ")");
    }

    /** Camera permission first (our own prompt), then the scanner screen. */
    private void startScan() {
        if (Build.VERSION.SDK_INT >= 23
                && checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[] {Manifest.permission.CAMERA}, REQ_CAMERA);
            return;
        }
        try {
            new IntentIntegrator(this)
                    .setDesiredBarcodeFormats(IntentIntegrator.QR_CODE)
                    .setPrompt("Point the camera at the QR code in Rustboard on your PC")
                    .setBeepEnabled(false)
                    .setOrientationLocked(false) // the app is landscape already (manifest)
                    .initiateScan();
        } catch (Throwable t) {
            scanError("Cannot open the scanner: " + t);
        }
    }

    @Override
    public void onRequestPermissionsResult(int code, String[] perms, int[] results) {
        super.onRequestPermissionsResult(code, perms, results);
        if (code != REQ_CAMERA) return;
        if (results.length > 0 && results[0] == PackageManager.PERMISSION_GRANTED) {
            startScan();
        } else {
            scanError("The camera is not allowed. Turn it on in Android Settings > Apps > Rustboard Remote > Permissions, or paste the link instead.");
        }
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        keepCrashes();
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        if (Build.VERSION.SDK_INT >= 28) {
            // use the whole screen, also beside the camera notch (the page pads itself)
            getWindow().getAttributes().layoutInDisplayCutoutMode =
                    WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
        }

        web = new WebView(this);
        web.setBackgroundColor(0xFF0B0B0C);
        setContentView(web);

        WebSettings s = web.getSettings();
        s.setJavaScriptEnabled(true);
        s.setDomStorageEnabled(true);
        s.setAllowFileAccess(true);
        s.setMixedContentMode(WebSettings.MIXED_CONTENT_ALWAYS_ALLOW);
        s.setBuiltInZoomControls(false);
        s.setSupportZoom(false);
        web.setOverScrollMode(View.OVER_SCROLL_NEVER);
        web.setWebViewClient(new WebViewClient() {
            @Override
            public void onPageFinished(WebView view, String url) {
                // a crash from the last run? the page shows it on a red screen
                String crash = readCrash();
                if (!crash.isEmpty()) js("window.onCrash && window.onCrash(" + JSONObject.quote(crash) + ")");
            }
        });
        web.addJavascriptInterface(new Bridge(), "Android");
        web.loadUrl("file:///android_asset/index.html");
    }

    /** What the page may ask the phone to do. */
    private class Bridge {
        @JavascriptInterface
        public void vibrate(int ms) {
            Vibrator v = (Vibrator) getSystemService(Context.VIBRATOR_SERVICE);
            if (v == null || !v.hasVibrator()) return;
            if (Build.VERSION.SDK_INT >= 26) {
                v.vibrate(VibrationEffect.createOneShot(ms, VibrationEffect.DEFAULT_AMPLITUDE));
            } else {
                v.vibrate(ms);
            }
        }

        /** Text on the phone clipboard (the link copied from the PC). */
        @JavascriptInterface
        public String paste() {
            ClipboardManager cm = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
            if (cm == null || !cm.hasPrimaryClip()) return "";
            ClipData clip = cm.getPrimaryClip();
            if (clip == null || clip.getItemCount() == 0) return "";
            CharSequence t = clip.getItemAt(0).coerceToText(MainActivity.this);
            return t == null ? "" : t.toString();
        }

        /** Open our own scanner screen (ZXing). The result comes back in onActivityResult. */
        @JavascriptInterface
        public void scanQr() {
            runOnUiThread(MainActivity.this::startScan);
        }

        /** Put text on the phone clipboard (the crash report). */
        @JavascriptInterface
        public void copy(String text) {
            runOnUiThread(() -> {
                ClipboardManager cm = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
                if (cm != null) cm.setPrimaryClip(ClipData.newPlainText("Rustboard crash", text));
            });
        }

        @JavascriptInterface
        public void clearCrash() {
            crashFile(MainActivity.this).delete();
        }

        @JavascriptInterface
        public void quit() {
            runOnUiThread(MainActivity.this::finish);
        }
    }

    /** The scanner screen is closed: give the text of the QR code to the page (empty = cancelled). */
    @Override
    @SuppressWarnings("deprecation")
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        IntentResult r = IntentIntegrator.parseActivityResult(requestCode, resultCode, data);
        if (r == null) {
            super.onActivityResult(requestCode, resultCode, data);
            return;
        }
        String text = r.getContents() == null ? "" : r.getContents();
        web.evaluateJavascript("window.onQr && window.onQr(" + JSONObject.quote(text) + ")", null);
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus) hideBars();
    }

    @SuppressWarnings("deprecation")
    private void hideBars() {
        getWindow().getDecorView().setSystemUiVisibility(
                View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                        | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
                        | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                        | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                        | View.SYSTEM_UI_FLAG_FULLSCREEN
                        | View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY);
    }

    /** Back: the page closes its own dialog first. If it has none, the app closes. */
    @Override
    @SuppressWarnings("deprecation")
    public void onBackPressed() {
        web.evaluateJavascript("(window.onBack ? window.onBack() : false)", value -> {
            if (!"true".equals(value)) finish();
        });
    }

    @Override
    protected void onResume() {
        super.onResume();
        web.onResume();
        web.evaluateJavascript("window.onWake && window.onWake()", null);
    }

    @Override
    protected void onPause() {
        web.onPause();
        super.onPause();
    }
}
