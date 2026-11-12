package dev.velmira.mobile;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.graphics.Color;
import android.net.Uri;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.text.InputType;
import android.view.Menu;
import android.view.MenuItem;
import android.view.View;
import android.view.Window;
import android.view.WindowManager;
import android.view.inputmethod.InputMethodManager;
import android.webkit.ValueCallback;
import android.webkit.WebChromeClient;
import android.webkit.WebResourceError;
import android.webkit.WebResourceRequest;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.Button;
import android.widget.EditText;
import android.widget.ProgressBar;
import android.widget.ScrollView;
import android.widget.TextView;
import android.widget.Toast;

import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.net.HttpURLConnection;
import java.net.URL;

/**
 * Velmira 的 Android 客户端：连接局域网里的 Velmira 桌面服务，用 WebView 显示同一套界面。
 * 手机与电脑需要在同一 Wi-Fi；地址形如 http://192.168.1.10:8765。
 */
public class MainActivity extends Activity {

    private static final String PREFS = "velmira";
    private static final String KEY_SERVER = "server";
    private static final int REQ_FILE = 1001;

    private WebView web;
    private ScrollView setup;
    private EditText serverInput;
    private TextView status;
    private ProgressBar progress;
    private Button connectBtn;

    private SharedPreferences prefs;
    private String baseUrl = "";
    private ValueCallback<Uri[]> fileCallback;

    private final Handler ui = new Handler(Looper.getMainLooper());

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setContentView(R.layout.activity_main);

        prefs = getSharedPreferences(PREFS, Context.MODE_PRIVATE);
        web = findViewById(R.id.web);
        setup = findViewById(R.id.setup);
        serverInput = findViewById(R.id.server);
        status = findViewById(R.id.status);
        progress = findViewById(R.id.progress);
        connectBtn = findViewById(R.id.connect);

        configureWebView();

        connectBtn.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                attemptConnect();
            }
        });
        serverInput.setOnEditorActionListener(new TextView.OnEditorActionListener() {
            @Override
            public boolean onEditorAction(TextView v, int actionId, android.view.KeyEvent event) {
                attemptConnect();
                return true;
            }
        });

        String saved = prefs.getString(KEY_SERVER, "");
        if (!saved.isEmpty()) {
            serverInput.setText(saved);
            serverInput.setSelection(saved.length());
            attemptConnect();
        } else {
            setup.setVisibility(View.VISIBLE);
            web.setVisibility(View.GONE);
        }
    }

    // ---------- WebView ----------

    private void configureWebView() {
        WebSettings s = web.getSettings();
        s.setJavaScriptEnabled(true);
        s.setDomStorageEnabled(true);
        s.setDatabaseEnabled(true);
        s.setAllowFileAccess(false);
        s.setAllowContentAccess(false);
        s.setSupportZoom(false);
        s.setBuiltInZoomControls(false);
        s.setLoadWithOverviewMode(false);
        s.setUseWideViewPort(true);
        s.setMediaPlaybackRequiresUserGesture(false);
        s.setMixedContentMode(WebSettings.MIXED_CONTENT_ALWAYS_ALLOW);
        s.setUserAgentString(s.getUserAgentString() + " VelmiraAndroid/0.4.0");
        if (getApplicationInfo().targetSdkVersion >= 26) {
            s.setSafeBrowsingEnabled(false);
        }
        web.setBackgroundColor(Color.rgb(244, 241, 234));
        web.setOverScrollMode(View.OVER_SCROLL_NEVER);

        web.setWebViewClient(new WebViewClient() {
            @Override
            public boolean shouldOverrideUrlLoading(WebView view, WebResourceRequest request) {
                Uri uri = request.getUrl();
                String scheme = uri.getScheme() == null ? "" : uri.getScheme();
                if ("http".equals(scheme) || "https".equals(scheme)) {
                    String host = uri.getHost() == null ? "" : uri.getHost();
                    String baseHost = Uri.parse(baseUrl).getHost();
                    if (host.equalsIgnoreCase(baseHost)) {
                        return false; // 应用内页面
                    }
                    try {
                        startActivity(new Intent(Intent.ACTION_VIEW, uri));
                    } catch (Exception ignored) {
                    }
                    return true;
                }
                return true;
            }

            @Override
            public void onReceivedError(WebView view, WebResourceRequest request, WebResourceError error) {
                if (request.isForMainFrame()) {
                    ui.post(new Runnable() {
                        @Override
                        public void run() {
                            progress.setVisibility(View.GONE);
                            showSetup();
                            setStatus("无法加载页面：" + error.getDescription());
                        }
                    });
                }
            }
        });

        web.setWebChromeClient(new WebChromeClient() {
            @Override
            public boolean onShowFileChooser(WebView view, ValueCallback<Uri[]> callback,
                                             FileChooserParams params) {
                if (fileCallback != null) {
                    fileCallback.onReceiveValue(null);
                }
                fileCallback = callback;
                try {
                    Intent intent = params.createIntent();
                    if (params.getMode() == FileChooserParams.MODE_OPEN_MULTIPLE) {
                        intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
                    }
                    startActivityForResult(Intent.createChooser(intent, "选择书籍文件"), REQ_FILE);
                    return true;
                } catch (Exception e) {
                    fileCallback = null;
                    Toast.makeText(MainActivity.this, "无法打开文件选择器", Toast.LENGTH_SHORT).show();
                    return false;
                }
            }
        });
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        if (requestCode == REQ_FILE) {
            if (fileCallback == null) {
                return;
            }
            Uri[] result = null;
            if (resultCode == RESULT_OK && data != null) {
                if (data.getClipData() != null) {
                    int n = data.getClipData().getItemCount();
                    result = new Uri[n];
                    for (int i = 0; i < n; i++) {
                        result[i] = data.getClipData().getItemAt(i).getUri();
                    }
                } else if (data.getData() != null) {
                    result = new Uri[]{data.getData()};
                }
            }
            fileCallback.onReceiveValue(result);
            fileCallback = null;
            return;
        }
        super.onActivityResult(requestCode, resultCode, data);
    }

    // ---------- 连接 ----------

    private void attemptConnect() {
        String candidate = normalize(serverInput.getText().toString());
        if (candidate == null) {
            setStatus("请输入服务地址，例如 http://192.168.1.10:8765");
            return;
        }
        hideKeyboard();
        connectBtn.setEnabled(false);
        progress.setVisibility(View.VISIBLE);
        setStatus("正在连接 " + candidate + " …");

        new Thread(new Runnable() {
            @Override
            public void run() {
                final boolean ok = probe(candidate);
                ui.post(new Runnable() {
                    @Override
                    public void run() {
                        connectBtn.setEnabled(true);
                        progress.setVisibility(View.GONE);
                        if (!ok) {
                            setStatus("连接失败：这个地址上没有运行 Velmira。请确认电脑端已启动服务、手机与电脑在同一 Wi-Fi。");
                            return;
                        }
                        baseUrl = candidate;
                        prefs.edit().putString(KEY_SERVER, candidate).apply();
                        showWeb();
                    }
                });
            }
        }, "velmira-probe").start();
    }

    private boolean probe(String base) {
        HttpURLConnection conn = null;
        try {
            URL url = new URL(base + "/api/health");
            conn = (HttpURLConnection) url.openConnection();
            conn.setConnectTimeout(5000);
            conn.setReadTimeout(5000);
            conn.setRequestMethod("GET");
            conn.setInstanceFollowRedirects(false);
            int code = conn.getResponseCode();
            if (code != 200) {
                return false;
            }
            InputStream in = conn.getInputStream();
            ByteArrayOutputStream buf = new ByteArrayOutputStream();
            byte[] chunk = new byte[4096];
            int read;
            while ((read = in.read(chunk)) > 0) {
                buf.write(chunk, 0, read);
                if (buf.size() > 65536) {
                    break;
                }
            }
            in.close();
            String body = buf.toString("UTF-8");
            return body.contains("\"ok\"") && body.contains("Velmira");
        } catch (Exception e) {
            return false;
        } finally {
            if (conn != null) {
                conn.disconnect();
            }
        }
    }

    private String normalize(String raw) {
        if (raw == null) {
            return null;
        }
        String s = raw.trim();
        if (s.isEmpty()) {
            return null;
        }
        if (!s.matches("^[a-zA-Z][a-zA-Z0-9+.-]*://.*")) {
            s = "http://" + s;
        }
        while (s.endsWith("/")) {
            s = s.substring(0, s.length() - 1);
        }
        if (!s.startsWith("http://") && !s.startsWith("https://")) {
            return null;
        }
        return s;
    }

    private void showWeb() {
        setup.setVisibility(View.GONE);
        web.setVisibility(View.VISIBLE);
        web.loadUrl(baseUrl + "/");
    }

    private void showSetup() {
        web.setVisibility(View.GONE);
        setup.setVisibility(View.VISIBLE);
        serverInput.setText(baseUrl.isEmpty() ? prefs.getString(KEY_SERVER, "") : baseUrl);
    }

    private void setStatus(String text) {
        status.setText(text);
    }

    private void hideKeyboard() {
        View focus = getCurrentFocus();
        if (focus != null) {
            InputMethodManager imm = (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
            if (imm != null) {
                imm.hideSoftInputFromWindow(focus.getWindowToken(), 0);
            }
        }
    }

    // ---------- 菜单 / 返回 ----------

    @Override
    public boolean onCreateOptionsMenu(Menu menu) {
        menu.add(Menu.NONE, 1, 0, "服务器设置");
        menu.add(Menu.NONE, 2, 1, "重新加载");
        return true;
    }

    @Override
    public boolean onOptionsItemSelected(MenuItem item) {
        if (item.getItemId() == 1) {
            showSetup();
            return true;
        }
        if (item.getItemId() == 2) {
            if (web.getVisibility() == View.VISIBLE) {
                web.reload();
            }
            return true;
        }
        return super.onOptionsItemSelected(item);
    }

    @Override
    public void onBackPressed() {
        if (web != null && web.getVisibility() == View.VISIBLE) {
            String url = web.getUrl();
            if (url != null && url.contains("?book=")) {
                web.loadUrl(baseUrl + "/");
                return;
            }
            if (web.canGoBack()) {
                web.goBack();
                return;
            }
            showSetup();
            return;
        }
        super.onBackPressed();
    }

    @Override
    protected void onDestroy() {
        if (web != null) {
            web.destroy();
        }
        super.onDestroy();
    }
}
