package dev.velmira.nativeapp;

import android.app.NativeActivity;
import android.app.AlertDialog;
import android.content.ContentResolver;
import android.content.DialogInterface;
import android.content.Intent;
import android.database.Cursor;
import android.graphics.Color;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.provider.OpenableColumns;
import android.text.InputFilter;
import android.text.InputType;
import android.util.Log;
import android.view.DisplayCutout;
import android.view.View;
import android.view.Window;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.WindowManager;
import android.window.OnBackInvokedCallback;
import android.window.OnBackInvokedDispatcher;
import android.widget.EditText;
import android.widget.FrameLayout;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.Locale;
import java.util.UUID;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

public final class MainActivity extends NativeActivity {
    private static final int BOOK_REQUEST = 1024;
    private static final String TAG = "Velmira";
    private static final int MAX_STEM_BYTES = 240;
    private static final long MIB = 1024L * 1024L;

    private final ExecutorService importWorker = Executors.newSingleThreadExecutor();

    // JNI invokes begin/take from the native thread. Activity callbacks run on the UI thread.
    private boolean importing;
    private boolean destroyed;
    private boolean handlesBack;
    private boolean readerBack;
    private File preparedBook;
    private String importResult;
    private volatile String textResult;
    private AlertDialog textDialog;
    private boolean lightTheme;
    private volatile long safeInsets;

    @Override
    public void onCreate(Bundle state) {
        Window window = getWindow();
        if (Build.VERSION.SDK_INT >= 30) {
            window.setDecorFitsSystemWindows(false);
        } else {
            window.getDecorView().setSystemUiVisibility(View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION);
        }
        window.setStatusBarColor(Color.TRANSPARENT);
        window.setNavigationBarColor(Color.TRANSPARENT);
        if (Build.VERSION.SDK_INT >= 28) {
            WindowManager.LayoutParams attributes = window.getAttributes();
            attributes.layoutInDisplayCutoutMode =
                WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
            window.setAttributes(attributes);
            window.setNavigationBarDividerColor(Color.TRANSPARENT);
        }
        if (Build.VERSION.SDK_INT >= 29) {
            window.setStatusBarContrastEnforced(false);
            window.setNavigationBarContrastEnforced(false);
        }
        super.onCreate(state);
        View decor = window.getDecorView();
        decor.setOnApplyWindowInsetsListener(new View.OnApplyWindowInsetsListener() {
            @Override
            public WindowInsets onApplyWindowInsets(View view, WindowInsets insets) {
                updateSafeInsets(insets);
                return view.onApplyWindowInsets(insets);
            }
        });
        WindowInsets initial = decor.getRootWindowInsets();
        if (initial != null) {
            updateSafeInsets(initial);
        }
        decor.requestApplyInsets();
        if (Build.VERSION.SDK_INT >= 33) {
            getOnBackInvokedDispatcher().registerOnBackInvokedCallback(
                OnBackInvokedDispatcher.PRIORITY_DEFAULT, new OnBackInvokedCallback() {
                    @Override
                    public void onBackInvoked() {
                        handleBack();
                    }
                });
        }
    }

    private void updateSafeInsets(WindowInsets insets) {
        int left, top, right, bottom;
        if (Build.VERSION.SDK_INT >= 30) {
            android.graphics.Insets safe = insets.getInsets(WindowInsets.Type.systemBars()
                | WindowInsets.Type.displayCutout() | WindowInsets.Type.ime());
            left = safe.left;
            top = safe.top;
            right = safe.right;
            bottom = safe.bottom;
        } else {
            left = insets.getSystemWindowInsetLeft();
            top = insets.getSystemWindowInsetTop();
            right = insets.getSystemWindowInsetRight();
            bottom = insets.getSystemWindowInsetBottom();
            if (Build.VERSION.SDK_INT >= 28) {
                DisplayCutout cutout = insets.getDisplayCutout();
                if (cutout != null) {
                    left = Math.max(left, cutout.getSafeInsetLeft());
                    top = Math.max(top, cutout.getSafeInsetTop());
                    right = Math.max(right, cutout.getSafeInsetRight());
                    bottom = Math.max(bottom, cutout.getSafeInsetBottom());
                }
            }
        }
        // Publish all four physical-pixel insets together, including a valid all-zero result.
        safeInsets = Long.MIN_VALUE | (Math.min(bottom, 0x7fffL) << 48)
            | (Math.min(right, 0xffffL) << 32) | (Math.min(top, 0xffffL) << 16)
            | Math.min(left, 0xffffL);
    }

    public long getSafeInsets() {
        return safeInsets;
    }

    public void setDarkSystemBarIcons(boolean dark) {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                lightTheme = dark;
                Window window = getWindow();
                if (Build.VERSION.SDK_INT >= 30) {
                    WindowInsetsController controller = window.getInsetsController();
                    if (controller != null) {
                        int mask = WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS
                            | WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS;
                        controller.setSystemBarsAppearance(dark ? mask : 0, mask);
                    }
                } else {
                    View decor = window.getDecorView();
                    int mask = View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR;
                    if (Build.VERSION.SDK_INT >= 26) {
                        mask |= View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR;
                    }
                    int flags = decor.getSystemUiVisibility() & ~mask;
                    decor.setSystemUiVisibility(dark ? flags | mask : flags);
                }
            }
        });
    }

    public synchronized void setHandlesBack(boolean enabled) {
        handlesBack = enabled;
        if (!enabled) {
            readerBack = false;
        }
    }

    public synchronized boolean takeReaderBack() {
        boolean back = readerBack;
        readerBack = false;
        return back;
    }

    public void editField(String title, String value, int kind, int limit) {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                showTextField(title, value, kind, limit);
            }
        });
    }

    private void showTextField(String title, String value, int kind, int limit) {
        if (isFinishing() || isDestroyed() || textDialog != null) {
            textResult = "cancel";
            return;
        }
        EditText field = new EditText(this);
        int type = InputType.TYPE_CLASS_TEXT;
        if (kind == 1) {
            type |= InputType.TYPE_TEXT_VARIATION_URI;
        } else if (kind == 2) {
            type |= InputType.TYPE_TEXT_VARIATION_PASSWORD;
        } else if (kind == 3) {
            type |= InputType.TYPE_TEXT_FLAG_MULTI_LINE;
        }
        field.setSingleLine(kind != 3);
        field.setInputType(type);
        field.setFilters(new InputFilter[] { new InputFilter.LengthFilter(limit) });
        field.setText(value);
        field.setSelection(field.length());
        FrameLayout container = new FrameLayout(this);
        int padding = Math.round(24 * getResources().getDisplayMetrics().density);
        container.setPadding(padding, 0, padding, 0);
        container.addView(field, new FrameLayout.LayoutParams(-1, -2));
        String[] reply = { "cancel" };
        int dialogTheme = lightTheme
            ? android.R.style.Theme_Material_Light_Dialog_Alert
            : android.R.style.Theme_Material_Dialog_Alert;
        textDialog = new AlertDialog.Builder(this, dialogTheme)
            .setTitle(title)
            .setView(container)
            .setNegativeButton("取消", null)
            .setPositiveButton("确定", new DialogInterface.OnClickListener() {
                @Override
                public void onClick(DialogInterface dialog, int which) {
                    reply[0] = "ok\n" + field.getText();
                }
            })
            .create();
        textDialog.setOnDismissListener(new DialogInterface.OnDismissListener() {
            @Override
            public void onDismiss(DialogInterface dialog) {
                textDialog = null;
                textResult = reply[0];
            }
        });
        field.requestFocus();
        textDialog.getWindow().setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_STATE_ALWAYS_VISIBLE);
        textDialog.show();
    }

    public String takeField() {
        String value = textResult;
        textResult = null;
        return value;
    }

    @Override
    public void onBackPressed() {
        handleBack();
    }

    private void handleBack() {
        synchronized (this) {
            if (handlesBack) {
                readerBack = true;
                return;
            }
        }
        // The native event loop belongs to this activity for the lifetime of the process.
        moveTaskToBack(true);
    }

    public void beginBookImport() {
        synchronized (this) {
            if (importing || destroyed) {
                return;
            }
            importing = true;
        }

        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                openDocumentPicker();
            }
        });
    }

    private void openDocumentPicker() {
        try {
            // Providers report EPUB and CBZ under inconsistent MIME types.
            Intent picker = new Intent(Intent.ACTION_OPEN_DOCUMENT)
                .addCategory(Intent.CATEGORY_OPENABLE)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                .setType("*/*");
            startActivityForResult(picker, BOOK_REQUEST);
        } catch (RuntimeException exception) {
            Log.w(TAG, "Unable to open the document picker", exception);
            completeImport(null, "error\nUnable to open the Android document picker.");
        }
    }

    public synchronized String takeBookImport() {
        if (preparedBook == null && importResult == null) {
            return null;
        }
        String result = preparedBook == null ? importResult : "ok\n" + preparedBook.getAbsolutePath();
        preparedBook = null;
        importResult = null;
        importing = false;
        return result;
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode != BOOK_REQUEST) {
            return;
        }
        if (resultCode != RESULT_OK) {
            completeImport(null, "cancel");
            return;
        }
        Uri uri = data == null ? null : data.getData();
        if (uri == null) {
            completeImport(null, "error\nThe document picker returned no file.");
            return;
        }
        importWorker.execute(new Runnable() {
            @Override
            public void run() {
                importBook(uri);
            }
        });
    }

    private void importBook(Uri uri) {
        try {
            completeImport(copyBook(uri), null);
        } catch (IOException | RuntimeException exception) {
            Log.w(TAG, "Unable to read the selected book", exception);
            String message = exception.getMessage();
            completeImport(null, "error\n" + (message == null ? "Unable to read the selected file." : message));
        }
    }

    @Override
    protected void onDestroy() {
        if (textDialog != null) {
            textDialog.dismiss();
        }
        File unclaimedBook;
        synchronized (this) {
            destroyed = true;
            unclaimedBook = preparedBook;
            preparedBook = null;
        }
        importWorker.shutdownNow();
        removeCopy(unclaimedBook);
        super.onDestroy();
    }

    private synchronized void completeImport(File file, String result) {
        if (!importing || destroyed) {
            removeCopy(file);
            return;
        }
        preparedBook = file;
        importResult = result;
    }

    private File copyBook(Uri uri) throws IOException {
        ContentResolver resolver = getContentResolver();
        String displayName = displayName(resolver, uri);
        if (displayName == null) {
            String segment = uri.getLastPathSegment();
            displayName = segment == null ? null : segment.substring(segment.lastIndexOf('/') + 1);
        }
        String extension = extensionFromName(displayName);
        String stem = displayName;
        if (extension == null) {
            extension = extensionFromMimeType(resolver.getType(uri));
            if (extension == null) {
                throw new IOException("Select a TXT, EPUB, or CBZ file.");
            }
        } else {
            stem = displayName.substring(0, displayName.length() - extension.length() - 1);
        }
        // Match the limits in the Rust importers.
        long limitMiB = extension.equals("txt") ? 64 : 512;
        long limit = limitMiB * MIB;

        File inbox = new File(getCacheDir(), "book-imports");
        if (!inbox.mkdirs() && !inbox.isDirectory()) {
            throw new IOException("Unable to open the book import cache.");
        }
        File directory = new File(inbox, UUID.randomUUID().toString());
        if (!directory.mkdir()) {
            throw new IOException("Unable to create a temporary book directory.");
        }
        File file = new File(directory, safeStem(stem) + "." + extension);
        boolean copied = false;
        try {
            try (InputStream input = resolver.openInputStream(uri)) {
                if (input == null) {
                    throw new IOException("The document provider could not open the selected file.");
                }
                try (FileOutputStream output = new FileOutputStream(file)) {
                    byte[] buffer = new byte[64 * 1024];
                    long total = 0;
                    while (!Thread.currentThread().isInterrupted()) {
                        int count = input.read(buffer);
                        if (count == -1) {
                            break;
                        }
                        total += count;
                        if (total > limit) {
                            throw new IOException("The selected file exceeds the " + limitMiB + " MiB limit.");
                        }
                        output.write(buffer, 0, count);
                    }
                    if (Thread.currentThread().isInterrupted()) {
                        throw new IOException("The book import was interrupted.");
                    }
                }
            }
            copied = true;
            return file;
        } finally {
            if (!copied) {
                removeCopy(file);
            }
        }
    }

    private static String displayName(ContentResolver resolver, Uri uri) {
        String[] columns = {OpenableColumns.DISPLAY_NAME};
        try (Cursor cursor = resolver.query(uri, columns, null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) {
                int index = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME);
                if (index >= 0) {
                    return cursor.getString(index);
                }
            }
        } catch (IllegalArgumentException | UnsupportedOperationException exception) {
            Log.d(TAG, "The document provider omitted the filename", exception);
        }
        return null;
    }

    private static String extensionFromName(String name) {
        if (name != null) {
            String lower = name.toLowerCase(Locale.ROOT);
            for (String extension : new String[]{"txt", "epub", "cbz"}) {
                if (lower.endsWith("." + extension)) {
                    return extension;
                }
            }
        }
        return null;
    }

    private static String extensionFromMimeType(String mimeType) {
        if (mimeType == null) {
            return null;
        }
        switch (mimeType.toLowerCase(Locale.ROOT)) {
            case "text/plain": return "txt";
            case "application/epub+zip": return "epub";
            case "application/vnd.comicbook+zip":
            case "application/x-cbz": return "cbz";
            default: return null;
        }
    }

    private static String safeStem(String name) {
        if (name == null || name.trim().isEmpty()) {
            return "Book";
        }
        String stem = name.trim().replace('/', '_').replace('\\', '_').replaceAll("\\p{Cntrl}", "_");
        byte[] utf8 = stem.getBytes(StandardCharsets.UTF_8);
        if (utf8.length <= MAX_STEM_BYTES) {
            return stem;
        }
        int end = MAX_STEM_BYTES;
        while ((utf8[end] & 0xc0) == 0x80) {
            end--;
        }
        return new String(utf8, 0, end, StandardCharsets.UTF_8);
    }

    private static void removeCopy(File file) {
        if (file != null) {
            file.delete();
            file.getParentFile().delete();
        }
    }
}
