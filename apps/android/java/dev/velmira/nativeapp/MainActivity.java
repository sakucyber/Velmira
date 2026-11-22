package dev.velmira.nativeapp;

import android.app.NativeActivity;
import android.content.ContentResolver;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.provider.OpenableColumns;
import android.util.Log;

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
    private boolean readingBook;
    private boolean readerBack;
    private File preparedBook;
    private String importResult;

    public synchronized void setReadingBook(boolean reading) {
        readingBook = reading;
        if (!reading) {
            readerBack = false;
        }
    }

    public synchronized boolean takeReaderBack() {
        boolean back = readerBack;
        readerBack = false;
        return back;
    }

    @Override
    public void onBackPressed() {
        synchronized (this) {
            if (readingBook) {
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
