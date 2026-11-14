package dev.velmira.mobile;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.graphics.Color;
import android.graphics.drawable.ColorDrawable;
import android.net.Uri;
import android.os.Bundle;
import android.os.Build;
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
import android.webkit.CookieManager;
import android.webkit.ConsoleMessage;
import android.webkit.WebChromeClient;
import android.webkit.WebResourceError;
import android.webkit.WebResourceRequest;
import android.webkit.WebResourceResponse;
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
import java.io.FileDescriptor;
import java.io.InputStream;
import java.io.PrintWriter;
import java.net.HttpURLConnection;
import java.net.URL;
import java.util.HashMap;
import java.util.Locale;
import java.util.Map;

/**
 * Standalone reader: its private library and HTTP service run on this device.
 * A desktop connection is optional and never gates opening the local library.
 */
public class MainActivity extends Activity {

    private static final String PREFS = "velmira";
    private static final String KEY_SERVER = "server";
    private static final String KEY_THEME = "window_theme";
    private static final String THEME_SIGNAL = "VELMIRA_WINDOW_THEME:";
    private static final int REQ_FILE = 1001;

    private WebView web;
    private ScrollView setup;
    private EditText serverInput;
    private TextView status;
    private ProgressBar progress;
    private Button connectBtn;

    private SharedPreferences prefs;
    private volatile String baseUrl = "";
    private String localOrigin = "";
    private String pairingFragment = "";
    private LocalServer localServer;
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

        applyWindowTheme(prefs.getString(KEY_THEME, "day"));
        configureWebView();
        progress.setVisibility(View.VISIBLE);

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

        Button local = findViewById(R.id.open_local);
        local.setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View view) { openLocalLibrary(); }
        });
        if (!handleConnectionIntent(getIntent())) openLocalLibrary();
    }

    // ---------- WebView ----------

    private void configureWebView() {
        // Pairing is handled by the shared login page. HttpOnly first-party
        // cookies also authenticate images and SSE; never store a pairing code.
        CookieManager.getInstance().setAcceptCookie(true);
        CookieManager.getInstance().setAcceptThirdPartyCookies(web, false);
        WebView.setWebContentsDebuggingEnabled((getApplicationInfo().flags & android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE) != 0);
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
        s.setMixedContentMode(WebSettings.MIXED_CONTENT_NEVER_ALLOW);
        s.setUserAgentString(s.getUserAgentString() + " VelmiraAndroid/0.5.1");
        if (Build.VERSION.SDK_INT >= 26) {
            s.setSafeBrowsingEnabled(false);
        }
        web.setOverScrollMode(View.OVER_SCROLL_NEVER);

        web.setWebViewClient(new WebViewClient() {
            @Override
            public WebResourceResponse shouldInterceptRequest(WebView view, WebResourceRequest request) {
                // Keep the configured HTTP(S) origin, cookie jar and IndexedDB.
                // Only static application assets come from the APK. API/media
                // requests still reach the server or the shared offline store.
                if (!"GET".equals(request.getMethod()) || !sameOrigin(request.getUrl())) {
                    return null;
                }
                return bundledAsset(request.getUrl());
            }

            @Override
            public boolean shouldOverrideUrlLoading(WebView view, WebResourceRequest request) {
                Uri uri = request.getUrl();
                String scheme = uri.getScheme() == null ? "" : uri.getScheme();
                if ("velmira".equals(scheme)) {
                    Uri current = view.getUrl() == null ? null : Uri.parse(view.getUrl());
                    if (!sameOrigin(current)) return true;
                    if ("connect".equals(uri.getHost())) showSetup();
                    else if ("home".equals(uri.getHost()) || "local".equals(uri.getHost())) openLocalLibrary();
                    return true;
                }
                if ("http".equals(scheme) || "https".equals(scheme)) {
                    String host = uri.getHost() == null ? "" : uri.getHost();
                    Uri base = Uri.parse(baseUrl);
                    String baseHost = base.getHost();
                    int port = uri.getPort() == -1 ? ("https".equals(scheme) ? 443 : 80) : uri.getPort();
                    int basePort = base.getPort() == -1 ? ("https".equals(base.getScheme()) ? 443 : 80) : base.getPort();
                    if (host.equalsIgnoreCase(baseHost) && scheme.equals(base.getScheme()) && port == basePort) {
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
            public void onPageFinished(WebView view, String url) {
                CookieManager.getInstance().flush();
                installThemeObserver(view);
            }

            @Override
            public void onReceivedError(WebView view, WebResourceRequest request, WebResourceError error) {
                if (request.isForMainFrame()) {
                    ui.post(new Runnable() {
                        @Override
                        public void run() {
                            progress.setVisibility(View.GONE);
                            if (baseUrl.equals(localOrigin)) showLocalError("本机界面加载失败，请重新打开。");
                            else { showSetup(); setStatus("电脑暂不可达，可返回本机书库。" ); }
                        }
                    });
                }
            }
        });

        web.setWebChromeClient(new WebChromeClient() {
            @Override
            public boolean onConsoleMessage(ConsoleMessage message) {
                String text = message.message();
                Uri current = web.getUrl() == null ? null : Uri.parse(web.getUrl());
                if (sameOrigin(current) && text.startsWith(THEME_SIGNAL)) {
                    String theme = text.substring(THEME_SIGNAL.length());
                    if ("day".equals(theme) || "night".equals(theme) || "sepia".equals(theme)) {
                        applyWindowTheme(theme);
                        return true;
                    }
                }
                return super.onConsoleMessage(message);
            }

            @Override
            public boolean onShowFileChooser(WebView view, ValueCallback<Uri[]> callback,
                                             FileChooserParams params) {
                if (fileCallback != null) {
                    fileCallback.onReceiveValue(null);
                }
                fileCallback = callback;
                try {
                    // WebView 会把 HTML 的 .txt/.epub/.cbz 扩展名原样作为 MIME 类型，
                    // 部分系统文档选择器因此把所有文件隐藏。由后端按内容验证格式。
                    Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
                    intent.addCategory(Intent.CATEGORY_OPENABLE);
                    intent.setType("*/*");
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

    // The page only signals one of three palette names. No JavaScript bridge,
    // account data or arbitrary native methods are exposed to book content.
    private void installThemeObserver(WebView view) {
        Uri current = view.getUrl() == null ? null : Uri.parse(view.getUrl());
        if (!sameOrigin(current)) return;
        view.evaluateJavascript("(function(){if(window.__velmiraWindowTheme)return;window.__velmiraWindowTheme=true;"
            + "function sync(){console.info('" + THEME_SIGNAL + "'+(document.documentElement.dataset.theme||'day'));}"
            + "new MutationObserver(sync).observe(document.documentElement,{attributes:true,attributeFilter:['data-theme']});sync();})()", null);
    }

    private void applyWindowTheme(String theme) {
        boolean dark = "night".equals(theme);
        int background = Color.parseColor(dark ? "#14161b" : "sepia".equals(theme) ? "#e9dcc2" : "#f4f1ea");
        int statusColor = Color.parseColor(dark ? "#1c1f26" : "sepia".equals(theme) ? "#f4e8d0" : "#fffdf8");
        Window window = getWindow();
        window.clearFlags(WindowManager.LayoutParams.FLAG_TRANSLUCENT_STATUS | WindowManager.LayoutParams.FLAG_TRANSLUCENT_NAVIGATION);
        window.addFlags(WindowManager.LayoutParams.FLAG_DRAWS_SYSTEM_BAR_BACKGROUNDS);
        window.setStatusBarColor(statusColor);
        window.setNavigationBarColor(background);
        window.setBackgroundDrawable(new ColorDrawable(background));
        if (Build.VERSION.SDK_INT >= 28) window.setNavigationBarDividerColor(background);
        if (Build.VERSION.SDK_INT >= 29) {
            window.setStatusBarContrastEnforced(false);
            window.setNavigationBarContrastEnforced(false);
        }
        // Keep the WebView within status/navigation/cutout insets. The web
        // safe-area padding remains zero here, and is used by browser/PWA mode.
        if (Build.VERSION.SDK_INT >= 30) window.setDecorFitsSystemWindows(true);
        View decor = window.getDecorView();
        int flags = decor.getSystemUiVisibility();
        flags &= ~(View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
            | View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR | View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR);
        if (!dark) {
            flags |= View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR;
            if (Build.VERSION.SDK_INT >= 26) flags |= View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR;
        }
        decor.setSystemUiVisibility(flags);
        if (web != null) web.setBackgroundColor(background);
        View content = findViewById(android.R.id.content);
        if (content != null) content.setBackgroundColor(background);
        prefs.edit().putString(KEY_THEME, theme).apply();
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

    private void openLocalLibrary() {
        try {
            if (localServer == null) localServer = new LocalServer(this);
            localOrigin = localServer.start();
            baseUrl = localOrigin;
            CookieManager.getInstance().setCookie(localOrigin, localServer.getCookie(), new ValueCallback<Boolean>() {
                @Override public void onReceiveValue(Boolean accepted) {
                    CookieManager.getInstance().flush(); showWeb();
                }
            });
        } catch (Exception error) {
            showLocalError("本机书库暂时无法启动，请重试。" );
        }
    }

    private void showLocalError(String message) {
        android.widget.LinearLayout screen = new android.widget.LinearLayout(this);
        screen.setOrientation(android.widget.LinearLayout.VERTICAL);
        screen.setPadding(32, 80, 32, 32);
        TextView text = new TextView(this); text.setText(message); text.setTextSize(18);
        Button retry = new Button(this); retry.setText("重新打开本机书库");
        retry.setOnClickListener(new View.OnClickListener() {
            @Override public void onClick(View view) { recreate(); }
        });
        screen.addView(text); screen.addView(retry); setContentView(screen);
    }

    private boolean sameOrigin(Uri uri) {
        if (baseUrl.isEmpty() || uri == null) return false;
        Uri base = Uri.parse(baseUrl);
        String scheme = uri.getScheme(), host = uri.getHost();
        if (scheme == null || host == null || uri.getUserInfo() != null) return false;
        int port = uri.getPort() == -1 ? ("https".equals(scheme) ? 443 : 80) : uri.getPort();
        int basePort = base.getPort() == -1 ? ("https".equals(base.getScheme()) ? 443 : 80) : base.getPort();
        return scheme.equals(base.getScheme()) && host.equalsIgnoreCase(base.getHost()) && port == basePort;
    }

    private WebResourceResponse bundledAsset(Uri uri) {
        String path = uri.getEncodedPath();
        if (path == null || path.isEmpty() || "/".equals(path)) path = "/index.html";
        // Never turn arbitrary URLs into AssetManager paths. Encoded path
        // separators, traversal, absolute paths and user supplied files fail
        // this allowlist before any APK asset is opened.
        if (!path.matches("/[A-Za-z0-9_./-]+") || path.contains("//")) return null;
        for (String part : path.split("/")) {
            if (".".equals(part) || "..".equals(part)) return null;
        }
        String lower = path.toLowerCase(Locale.ROOT);
        String mime = null;
        if ("/index.html".equals(path)) mime = "text/html";
        else if ("/app.js".equals(path) || "/sw.js".equals(path) ||
                ((path.startsWith("/ui/") || path.startsWith("/util/")) && lower.endsWith(".js"))) mime = "application/javascript";
        else if (path.startsWith("/styles/") && lower.endsWith(".css")) mime = "text/css";
        else if ("/manifest.webmanifest".equals(path)) mime = "application/manifest+json";
        else if (path.startsWith("/icons/")) {
            if (lower.endsWith(".png")) mime = "image/png";
            else if (lower.endsWith(".svg")) mime = "image/svg+xml";
            else if (lower.endsWith(".ico")) mime = "image/x-icon";
            else if (lower.endsWith(".webp")) mime = "image/webp";
        }
        if (mime == null) return null;
        try {
            InputStream stream = getAssets().open("web" + path);
            Map<String, String> headers = new HashMap<String, String>();
            headers.put("Cache-Control", "no-cache");
            headers.put("X-Content-Type-Options", "nosniff");
            String encoding = mime.startsWith("image/") ? null : "UTF-8";
            return new WebResourceResponse(mime, encoding, 200, "OK", headers, stream);
        } catch (java.io.IOException missing) {
            return null;
        }
    }

    private void attemptConnect() {
        String raw = serverInput.getText().toString().trim();
        String fragment = "";
        try {
            Uri pasted = Uri.parse(raw);
            String value = pasted.getFragment();
            if (value != null && value.matches("pair=[A-Za-z0-9_-]{16,200}")) {
                fragment = "#" + value;
                raw = pasted.buildUpon().fragment(null).build().toString();
            }
        } catch (Exception ignored) { }
        String candidate = normalize(raw);
        if (candidate == null) {
            setStatus("请输入服务地址，例如 http://192.168.1.10:8765");
            return;
        }
        hideKeyboard();
        connectBtn.setEnabled(false);
        progress.setVisibility(View.VISIBLE);
        setStatus("正在连接 " + candidate + " …");
        // Opening the bundled application does not require a running server.
        // Its local store decides which books are available offline.
        baseUrl = candidate;
        pairingFragment = fragment;
        prefs.edit().putString(KEY_SERVER, candidate).apply();
        showWeb();

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
                            setStatus("服务暂不可达，已打开离线界面。可阅读此前缓存的书籍；首次使用请在电脑服务可达时配对和缓存。");
                            Toast.makeText(MainActivity.this, "服务暂不可达，可继续阅读已缓存书籍", Toast.LENGTH_LONG).show();
                            return;
                        }
                        setStatus("服务可达，书库会自动同步。");
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
        Uri address = Uri.parse(s);
        if (address.getHost() == null || address.getHost().isEmpty() || address.getUserInfo() != null ||
                address.getQuery() != null || address.getFragment() != null ||
                (address.getPath() != null && !address.getPath().isEmpty() && !"/".equals(address.getPath())) ||
                address.getPort() < -1 || address.getPort() == 0 || address.getPort() > 65535) {
            return null;
        }
        return s;
    }

    private void showWeb() {
        progress.setVisibility(View.GONE);
        setup.setVisibility(View.GONE);
        web.setVisibility(View.VISIBLE);
        String suffix = pairingFragment;
        pairingFragment = ""; // Never persist or reuse a one-time invitation.
        web.loadUrl(baseUrl + "/" + suffix);
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        handleConnectionIntent(intent);
    }

    private boolean handleConnectionIntent(Intent intent) {
        if (intent == null || !Intent.ACTION_SEND.equals(intent.getAction()) || !"text/plain".equals(intent.getType())) return false;
        String shared = intent.getStringExtra(Intent.EXTRA_TEXT);
        if (shared == null || shared.length() > 2048) return false;
        Uri uri;
        try { uri = Uri.parse(shared.trim()); } catch (Exception invalid) { return false; }
        if (uri.getFragment() == null || !uri.getFragment().matches("pair=[A-Za-z0-9_-]{16,200}")) return false;
        String base = uri.buildUpon().fragment(null).build().toString();
        if (normalize(base) == null) return false;
        showSetup();
        serverInput.setText(shared.trim());
        setStatus("已收到电脑配对链接。点击连接即可配对，链接仅使用一次，不会保存。");
        return true;
    }

    private void showSetup() {
        web.setVisibility(View.GONE);
        setup.setVisibility(View.VISIBLE);
        serverInput.setText(prefs.getString(KEY_SERVER, ""));
        setStatus("可选：粘贴电脑生成的连接链接，或填写服务地址。手机本地阅读无需连接电脑。" );
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
        menu.add(Menu.NONE, 3, 0, "本机书库");
        menu.add(Menu.NONE, 1, 1, "连接电脑（可选）");
        menu.add(Menu.NONE, 2, 1, "重新加载");
        return true;
    }

    @Override
    public boolean onOptionsItemSelected(MenuItem item) {
        if (item.getItemId() == 1) {
            showSetup();
            return true;
        }
        if (item.getItemId() == 3) { openLocalLibrary(); return true; }
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
            if (!baseUrl.equals(localOrigin)) { openLocalLibrary(); return; }
            super.onBackPressed(); return;
        }
        // A QR scanner may share the one-time invitation while the app is
        // closed. Consume it before opening the local library so the user can
        // confirm the desktop connection instead of losing the fragment.
        if (!handleConnectionIntent(getIntent())) openLocalLibrary();
    }

    @Override
    public void dump(String prefix, FileDescriptor fd, PrintWriter writer, String[] args) {
        super.dump(prefix, fd, writer, args);
        Window window = getWindow();
        writer.println(prefix + "Velmira window theme:");
        writer.println(prefix + "  theme=" + (prefs == null ? "day" : prefs.getString(KEY_THEME, "day")));
        writer.println(prefix + "  statusBarColor=" + String.format(Locale.ROOT, "#%08x", window.getStatusBarColor()));
        writer.println(prefix + "  navigationBarColor=" + String.format(Locale.ROOT, "#%08x", window.getNavigationBarColor()));
        writer.println(prefix + "  systemUiVisibility=" + String.format(Locale.ROOT, "0x%08x", window.getDecorView().getSystemUiVisibility()));
    }

    @Override
    protected void onDestroy() {
        if (web != null) {
            web.destroy();
        }
        if (localServer != null) localServer.close();
        super.onDestroy();
    }
}
