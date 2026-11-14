package dev.velmira.mobile;

import android.content.ContentValues;
import android.content.Context;
import android.content.SharedPreferences;
import android.database.Cursor;
import android.database.sqlite.SQLiteDatabase;
import android.database.sqlite.SQLiteOpenHelper;
import org.json.JSONArray;
import org.json.JSONObject;
import java.io.*;
import java.net.*;
import java.nio.charset.Charset;
import java.security.SecureRandom;
import java.util.*;
import java.util.concurrent.*;

/** Small loopback-only reader service. SQLite and all imported files live in
 * Android's application-private directory; no desktop process is required. */
public final class LocalServer {
    public static final String COOKIE_NAME="velmira_local";
    private final Context context;
    private final File booksRoot;
    private final Store helper;
    private final LocalAiService mobileAi;
    private final String session;
    private ServerSocket listener;
    private final ExecutorService workers=Executors.newFixedThreadPool(12);
    private volatile boolean running=false;
    private String origin;
    private int revision=0;
    public LocalServer(Context context) {
        this.context=context.getApplicationContext(); booksRoot=new File(context.getFilesDir(),"local-books");booksRoot.mkdirs();
        helper=new Store(this.context);mobileAi=new LocalAiService(this.context);byte[] bytes=new byte[32];new SecureRandom().nextBytes(bytes);StringBuilder value=new StringBuilder();for(byte b:bytes)value.append(String.format(Locale.ROOT,"%02x",b&255));session=value.toString();
    }
    public synchronized String start() throws IOException {
        if(running)return origin;
        SharedPreferences prefs=context.getSharedPreferences("velmira",Context.MODE_PRIVATE);int preferred=prefs.getInt("local_port",18765);
        listener=new ServerSocket();listener.setReuseAddress(true);
        try {listener.bind(new InetSocketAddress(InetAddress.getByName("127.0.0.1"),preferred),20);}
        catch(IOException occupied){listener.close();listener=new ServerSocket();listener.bind(new InetSocketAddress(InetAddress.getByName("127.0.0.1"),0),20);}
        origin="http://127.0.0.1:"+listener.getLocalPort();prefs.edit().putInt("local_port",listener.getLocalPort()).apply();
        try {helper.getWritableDatabase();}
        catch(RuntimeException failed){try{listener.close();}catch(IOException ignored){}throw failed;}
        running=true;
        Thread accept=new Thread(new Runnable(){@Override public void run(){while(running){try {final Socket socket=listener.accept();workers.execute(new Runnable(){@Override public void run(){serve(socket);}});}catch(Exception error){if(!running)break;}}}},"velmira-local");accept.setDaemon(true);accept.start();return origin;
    }
    public String cookie() {return COOKIE_NAME+"="+session+"; Path=/; HttpOnly; SameSite=Strict";}
    public String getCookie() {return cookie();}
    public String origin() {return origin;}
    public synchronized void stop(){running=false;try{if(listener!=null)listener.close();}catch(IOException ignored){}workers.shutdownNow();helper.close();}
    public void close(){stop();}
    private static final class Store extends SQLiteOpenHelper {
        Store(Context context){super(context,"velmira-local-reader.db",null,1);setWriteAheadLoggingEnabled(true);}
        @Override public void onCreate(SQLiteDatabase db){
            db.execSQL("CREATE TABLE books(id TEXT PRIMARY KEY,sha TEXT UNIQUE,json TEXT NOT NULL,archived INTEGER)");
            db.execSQL("CREATE TABLE chapters(book TEXT,idx INTEGER,json TEXT NOT NULL,PRIMARY KEY(book,idx))");
            db.execSQL("CREATE TABLE media(book TEXT,id TEXT,type TEXT,file TEXT,width INTEGER,height INTEGER,chapter INTEGER,PRIMARY KEY(book,id))");
            db.execSQL("CREATE TABLE records(kind TEXT,id TEXT,book TEXT,json TEXT NOT NULL,PRIMARY KEY(kind,id))");
            db.execSQL("CREATE INDEX records_book ON records(book,kind)");
        }
        @Override public void onUpgrade(SQLiteDatabase db,int oldVersion,int newVersion){}
    }
    private static final class Request {
        String method,path; Map<String,String> headers=new HashMap<>();Map<String,String> query=new HashMap<>();byte[] body;File upload;
        JSONObject json() throws Exception {if(body==null||body.length==0)throw new HttpError(400,"invalid_json","需要 JSON 对象");return new JSONObject(new String(body,"UTF-8"));}
    }
    private static final class Response {
        int status=200;String type="application/json; charset=utf-8";byte[] bytes;InputStream stream;long length=-1;String disposition;
        Response(JSONObject value){bytes=value.toString().getBytes(Charset.forName("UTF-8"));}
        Response(int status,JSONObject value){this(value);this.status=status;}
        Response(String type,InputStream stream,long length){this.type=type;this.stream=stream;this.length=length;}
    }
    private static final class HttpError extends Exception {
        final int status;final String code;HttpError(int status,String code,String message){super(message);this.status=status;this.code=code;}
    }
    private void serve(Socket socket) {
        Request request=null;
        try(Socket client=socket) {
            client.setSoTimeout(20000);InputStream input=new BufferedInputStream(client.getInputStream());OutputStream output=new BufferedOutputStream(client.getOutputStream());
            try {request=readRequest(input);Response response=dispatch(request);writeResponse(output,response,request.method.equals("HEAD"));}
            catch(HttpError error){writeResponse(output,new Response(error.status,error(error.code,error.getMessage())),false);}
            catch(LocalAiService.AiException error){writeResponse(output,new Response(error.status,error(error.code,error.getMessage())),false);}
            catch(Exception error){writeResponse(output,new Response(400,error("local_request_failed",error instanceof IOException?error.getMessage():"请求无法完成，请检查文件或重试")),false);}
            output.flush();
        }catch(Exception ignored){}finally{if(request!=null&&request.upload!=null)request.upload.delete();}
    }
    private Request readRequest(InputStream input) throws Exception {
        Request request=new Request();String first=line(input,8192);if(first==null)throw new IOException("空请求");String[] values=first.split(" ");
        if(values.length!=3||!values[2].equals("HTTP/1.1"))throw new HttpError(400,"invalid_request","请求格式无效");
        request.method=values[0];String target=values[1];if(!target.startsWith("/")||target.startsWith("//"))throw new HttpError(400,"invalid_path","请求路径无效");
        int total=0;for(String header;(header=line(input,8192))!=null&&!header.isEmpty();) {
            total+=header.length();if(total>32768)throw new HttpError(431,"headers_too_large","请求头过大");int split=header.indexOf(':');if(split<1)throw new IOException("请求头无效");
            String key=header.substring(0,split).trim().toLowerCase(Locale.ROOT);if(request.headers.containsKey(key)&&!key.equals("cookie"))throw new HttpError(400,"duplicate_header","重复请求头");
            request.headers.put(key,header.substring(split+1).trim());
        }
        String expected="127.0.0.1:"+listener.getLocalPort();if(!expected.equals(request.headers.get("host")))throw new HttpError(403,"host_rejected","仅允许应用本机访问");
        String rawPath=target.split("\\?",2)[0];request.path=URLDecoder.decode(rawPath.replace("+","%2B"),"UTF-8");
        for(String part:request.path.split("/"))if(part.equals(".")||part.equals(".."))throw new HttpError(400,"invalid_path","路径无效");
        if(request.path.indexOf('\\')>=0||request.path.indexOf('\0')>=0)throw new HttpError(400,"invalid_path","路径无效");
        if(target.indexOf('?')>=0)for(String pair:target.substring(target.indexOf('?')+1).split("&")){String[] kv=pair.split("=",2);request.query.put(URLDecoder.decode(kv[0],"UTF-8"),kv.length>1?URLDecoder.decode(kv[1],"UTF-8"):"");}
        boolean mutation=!request.method.equals("GET")&&!request.method.equals("HEAD");
        String caller=request.headers.get("origin");
        if(caller!=null&&!origin.equals(caller))throw new HttpError(403,"origin_rejected","仅允许本应用发起请求");
        if(mutation&&caller==null)throw new HttpError(403,"origin_rejected","写入需要同源请求");
        boolean publicApi=request.path.equals("/api/health")||request.path.equals("/api/auth/status");
        if((request.path.startsWith("/api/")||request.path.startsWith("/media/"))&&!publicApi) {
            String cookie=request.headers.get("cookie");boolean valid=false;
            if(cookie!=null)for(String part:cookie.split(";"))if((COOKIE_NAME+"="+session).equals(part.trim()))valid=true;
            if(!valid)throw new HttpError(401,"local_authentication_required","请从 Velmira 应用打开书库");
        }
        if(request.headers.containsKey("transfer-encoding"))throw new HttpError(400,"invalid_encoding","请求编码不受支持");
        long length=0;if(request.headers.containsKey("content-length"))try{length=Long.parseLong(request.headers.get("content-length"));}catch(NumberFormatException error){throw new IOException("长度无效");}
        if(length<0||length>LocalBookImporter.MAX_SOURCE)throw new HttpError(413,"upload_too_large","单文件不能超过 64MB");
        boolean importFile=request.path.equals("/api/books")&&request.method.equals("POST");
        if(!importFile&&length>1024*1024)throw new HttpError(413,"body_too_large","请求内容过大");
        if(importFile) {
            request.upload=File.createTempFile("book-upload-",".bin",context.getCacheDir());
            try(OutputStream file=new FileOutputStream(request.upload)){copyExact(input,file,length);}
            catch(Exception failure){request.upload.delete();throw failure;}
        } else if(length>0){ByteArrayOutputStream bytes=new ByteArrayOutputStream((int)length);copyExact(input,bytes,length);request.body=bytes.toByteArray();}
        return request;
    }
    private static String line(InputStream input,int limit)throws IOException {ByteArrayOutputStream data=new ByteArrayOutputStream();for(int value;(value=input.read())!=-1;){if(value=='\n')return new String(data.toByteArray(),"ISO-8859-1").replaceFirst("\\r$","");if(data.size()>=limit)throw new IOException("请求行过长");data.write(value);}return data.size()==0?null:new String(data.toByteArray(),"ISO-8859-1");}
    private static void copyExact(InputStream input,OutputStream output,long length)throws IOException {byte[] buffer=new byte[16384];long remaining=length;while(remaining>0){int n=input.read(buffer,0,(int)Math.min(buffer.length,remaining));if(n<0)throw new EOFException("文件上传不完整");output.write(buffer,0,n);remaining-=n;}}
    private static JSONObject object(String key,Object value)throws Exception{return new JSONObject().put(key,value);}
    private static JSONObject error(String code,String message){try{return object("error",new JSONObject().put("code",code).put("message",message==null?"请求失败":message));}catch(Exception impossible){return new JSONObject();}}
    private static void writeResponse(OutputStream output,Response response,boolean head)throws IOException {
        long length=response.bytes!=null?response.bytes.length:response.length;
        String reason=response.status>=400?"Error":"OK";
        String headers="HTTP/1.1 "+response.status+" "+reason+"\r\nContent-Type: "+response.type+"\r\nContent-Length: "+length+"\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\n";
        if(response.disposition!=null)headers+="Content-Disposition: "+response.disposition+"\r\n";
        output.write((headers+"\r\n").getBytes(Charset.forName("ISO-8859-1")));
        try{if(!head){if(response.bytes!=null)output.write(response.bytes);else if(response.stream!=null){byte[] buffer=new byte[16384];for(int n;(n=response.stream.read(buffer))!=-1;)output.write(buffer,0,n);}}}finally{if(response.stream!=null)response.stream.close();}
    }
    private Response dispatch(Request request)throws Exception {
        if(request.path.startsWith("/api/mobile-ai/images/")) {
            if(!request.method.equals("GET")&&!request.method.equals("HEAD"))throw method();
            String name=request.path.substring("/api/mobile-ai/images/".length());return file(mobileAi.imageType(name),mobileAi.openImage(name));
        }
        if(request.path.startsWith("/api/mobile-ai/")) {
            JSONObject body=request.body==null?new JSONObject():request.json();
            JSONObject value=mobileAi.handle(request.method,request.path,request.query,body);if(value==null)throw missing();return json(value);
        }
        synchronized(this) {return dispatchReader(request);}
    }
    private Response dispatchReader(Request request)throws Exception {
        String path=request.path,method=request.method;SQLiteDatabase db=helper.getWritableDatabase();
        if(path.equals("/api/health"))return json(new JSONObject().put("ok",true).put("name","Velmira").put("version","0.5.1").put("standalone",true));
        if(path.equals("/api/auth/status"))return json(new JSONObject().put("local",true).put("authenticated",true).put("enabled",false).put("standalone",true));
        if(path.equals("/api/events/status"))return json(new JSONObject().put("revision",revision).put("subscribers",0));
        if(path.equals("/api/events"))return new Response(204,new JSONObject());
        if(path.equals("/api/books")) {
            if(method.equals("POST"))return importBook(db,request);
            if(!method.equals("GET")&&!method.equals("HEAD"))throw method();
            boolean archived="true".equals(request.query.get("archived"));JSONArray books=new JSONArray();
            try(Cursor cursor=db.rawQuery("SELECT json,archived FROM books WHERE archived IS "+(archived?"NOT NULL":"NULL")+" ORDER BY rowid DESC",null)){
                while(cursor.moveToNext()){JSONObject book=new JSONObject(cursor.getString(0));decorateBook(db,book,cursor.isNull(1)?null:cursor.getLong(1));books.put(book);}
            }return json(object("books",books));
        }
        if(path.startsWith("/media/")){String[] parts=path.split("/");if(parts.length!=4)throw missing();return media(db,parts[2],parts[3]);}
        if(path.startsWith("/api/books/")) {
            String[] parts=path.split("/");if(parts.length<4)throw missing();String bookId=parts[3];JSONObject book=book(db,bookId);
            if(parts.length==4){
                if(method.equals("PATCH")){JSONObject payload=request.json();ContentValues update=new ContentValues();if(payload.has("title")){String title=payload.getString("title").trim();if(title.isEmpty()||title.length()>200)throw invalid("书名须为 1–200 个字符");book.put("title",title);update.put("json",book.toString());}
                    if(payload.has("archived")){if(!(payload.get("archived")instanceof Boolean))throw invalid("archived 必须为布尔值");if(payload.getBoolean("archived"))update.put("archived",now());else update.putNull("archived");}
                    if(update.size()==0)throw invalid("仅支持书名和移出设置");db.update("books",update,"id=?",new String[]{bookId});revision++;return json(object("book",book));}
                if(!method.equals("GET")&&!method.equals("HEAD"))throw method();return json(object("book",book));
            }
            String resource=parts[4];
            if(resource.equals("source")&&method.equals("GET")){File file=safeFile(bookId,"source");if(!file.isFile())throw missing();Response response=file("application/octet-stream",file);response.disposition="attachment; filename=book."+book.getString("format");return response;}
            if(resource.equals("toc")&&method.equals("GET")){JSONArray chapters=new JSONArray();try(Cursor cursor=db.rawQuery("SELECT json FROM chapters WHERE book=? ORDER BY idx",new String[]{bookId})){while(cursor.moveToNext()){JSONObject ch=new JSONObject(cursor.getString(0));chapters.put(new JSONObject().put("idx",ch.getInt("idx")).put("title",ch.getString("title")).put("chars",ch.getLong("chars")));}}return json(new JSONObject().put("book_id",bookId).put("chapters",chapters));}
            if(resource.equals("chapters")&&parts.length==6&&method.equals("GET"))return json(chapter(db,bookId,number(parts[5],0,100000)));
            if(resource.equals("pages")&&method.equals("GET"))return pages(db,bookId,book,parts,request);
            if(resource.equals("epub")&&parts.length==7&&parts[5].equals("assets")&&method.equals("GET"))return media(db,bookId,parts[6]);
            if(resource.equals("progress"))return progress(db,request,bookId,book);
            if(resource.equals("bookmarks"))return bookmarks(db,request,bookId,parts);
            if(resource.equals("annotations"))return annotations(db,request,bookId,parts);
            if(resource.equals("search")&&method.equals("GET"))return search(db,request,bookId);
            if(resource.equals("segments"))return json(object("segments",new JSONArray()));
            if(resource.equals("ai")&&parts.length>=6&&(parts[5].equals("scenes")||parts[5].equals("characters")))return json(object(parts[5],new JSONArray()));
            throw unavailable();
        }
        if(path.startsWith("/api/"))throw unavailable();
        if(!method.equals("GET")&&!method.equals("HEAD"))throw method();return asset(path);
    }
    private Response importBook(SQLiteDatabase db,Request request)throws Exception {
        if(request.upload==null)throw invalid("请选择书籍文件");String sha=LocalBookImporter.sha256(request.upload);
        try(Cursor cursor=db.rawQuery("SELECT id,json,archived FROM books WHERE sha=?",new String[]{sha})){
            if(cursor.moveToFirst()){String id=cursor.getString(0);boolean restored=!cursor.isNull(2);ContentValues values=new ContentValues();values.putNull("archived");db.update("books",values,"id=?",new String[]{id});revision++;return json(new JSONObject().put("book",new JSONObject(cursor.getString(1))).put("deduped",true).put("restored",restored));}
        }
        File staging=new File(booksRoot,"import_"+UUID.randomUUID().toString().replace("-",""));if(!staging.mkdirs())throw new IOException("无法创建书籍目录");boolean committed=false;
        try {
            File source=new File(staging,"source");try(InputStream input=new FileInputStream(request.upload)){LocalBookImporter.copy(input,source,LocalBookImporter.MAX_SOURCE);}
            LocalBookImporter.Parsed parsed=LocalBookImporter.parse(source,request.query.get("filename"),staging);String id=parsed.book.getString("id");File destination=safeFile(id,"");
            if(destination.exists()||!staging.renameTo(destination))throw new IOException("无法保存书籍文件");
            try{db.beginTransaction();ContentValues values=new ContentValues();values.put("id",id);values.put("sha",sha);values.put("json",parsed.book.toString());db.insertOrThrow("books",null,values);
                for(JSONObject ch:parsed.chapters){ContentValues row=new ContentValues();row.put("book",id);row.put("idx",ch.getInt("idx"));row.put("json",ch.toString());db.insertOrThrow("chapters",null,row);}
                for(LocalBookImporter.Media image:parsed.media){ContentValues row=new ContentValues();row.put("book",id);row.put("id",image.id);row.put("type",image.type);row.put("file",image.file.getName());row.put("width",image.width);row.put("height",image.height);row.put("chapter",image.chapter);db.insertOrThrow("media",null,row);}
                db.setTransactionSuccessful();committed=true;
            }finally{db.endTransaction();if(!committed)deleteTree(destination);}
            revision++;return new Response(201,new JSONObject().put("book",parsed.book).put("deduped",false).put("restored",false));
        }finally{if(staging.exists())deleteTree(staging);}
    }
    private void decorateBook(SQLiteDatabase db,JSONObject book,Long archived)throws Exception {
        JSONObject progress=record(db,"progress",book.getString("id"));boolean has=progress!=null;
        book.put("last_chapter",has?progress.optInt("chapter"):0).put("percent",has?progress.optDouble("percent"):book.getString("format").equals("cbz")?1.0/book.getInt("chapter_count"):0)
            .put("has_progress",has).put("last_read_at",has?progress.optLong("updated_at"):0).put("archived_at",archived==null?JSONObject.NULL:archived);
    }
    private JSONObject book(SQLiteDatabase db,String id)throws Exception {try(Cursor cursor=db.rawQuery("SELECT json FROM books WHERE id=?",new String[]{id})){if(!cursor.moveToFirst())throw new HttpError(404,"book_not_found","书籍不存在");return new JSONObject(cursor.getString(0));}}
    private JSONObject chapter(SQLiteDatabase db,String id,int index)throws Exception {try(Cursor cursor=db.rawQuery("SELECT json FROM chapters WHERE book=? AND idx=?",new String[]{id,String.valueOf(index)})){if(!cursor.moveToFirst())throw new HttpError(404,"chapter_not_found","章节不存在");return new JSONObject(cursor.getString(0));}}
    private JSONObject record(SQLiteDatabase db,String kind,String id)throws Exception {try(Cursor cursor=db.rawQuery("SELECT json FROM records WHERE kind=? AND id=?",new String[]{kind,id})){return cursor.moveToFirst()?new JSONObject(cursor.getString(0)):null;}}
    private void saveRecord(SQLiteDatabase db,String kind,String id,String book,JSONObject value){ContentValues row=new ContentValues();row.put("kind",kind);row.put("id",id);row.put("book",book);row.put("json",value.toString());db.insertWithOnConflict("records",null,row,SQLiteDatabase.CONFLICT_REPLACE);}
    private List<JSONObject> records(SQLiteDatabase db,String kind,String book)throws Exception {List<JSONObject> records=new ArrayList<>();try(Cursor cursor=db.rawQuery("SELECT json FROM records WHERE kind=? AND book=? ORDER BY rowid",new String[]{kind,book})){while(cursor.moveToNext())records.add(new JSONObject(cursor.getString(0)));}return records;}
    private Response progress(SQLiteDatabase db,Request request,String id,JSONObject book)throws Exception {
        JSONObject progress;
        if(request.method.equals("PUT")){JSONObject input=request.json();int chapter=integer(input,"chapter",0),para=integer(input,"para",0),offset=integer(input,"offset",0);validateLocation(db,id,book,chapter,para,offset);progress=new JSONObject().put("chapter",chapter).put("para",para).put("offset",offset).put("percent",percent(db,id,book,chapter,para,offset)).put("updated_at",now());saveRecord(db,"progress",id,id,progress);}
        else if(request.method.equals("GET")){progress=record(db,"progress",id);if(progress==null)progress=new JSONObject().put("chapter",0).put("para",0).put("offset",0).put("percent",book.getString("format").equals("cbz")?1.0/book.getInt("chapter_count"):0).put("updated_at",0);}
        else throw method();return json(object("progress",progress));
    }
    private void validateLocation(SQLiteDatabase db,String id,JSONObject book,int chapter,int para,int offset)throws Exception {
        if(chapter>=book.getInt("chapter_count"))throw invalid("章节位置超出范围");if(book.getString("format").equals("cbz")){if(para!=0||offset!=0)throw invalid("漫画位置无效");return;}
        JSONArray paragraphs=chapter(db,id,chapter).getJSONArray("paragraphs");if(paragraphs.length()==0){if(para!=0||offset!=0)throw invalid("图片章节位置无效");return;}if(para>=paragraphs.length())throw invalid("段落位置超出范围");String text=paragraphs.getString(para);if(offset>text.codePointCount(0,text.length()))throw invalid("字符位置超出范围");
    }
    private double percent(SQLiteDatabase db,String id,JSONObject book,int chapter,int para,int offset)throws Exception {
        if(book.getString("format").equals("cbz"))return (chapter+1.0)/book.getInt("chapter_count");long before=offset;
        try(Cursor cursor=db.rawQuery("SELECT json FROM chapters WHERE book=? AND idx<?",new String[]{id,String.valueOf(chapter)})){while(cursor.moveToNext())before+=new JSONObject(cursor.getString(0)).getLong("chars");}
        JSONArray paragraphs=chapter(db,id,chapter).getJSONArray("paragraphs");for(int i=0;i<para;i++){String text=paragraphs.getString(i);before+=text.codePointCount(0,text.length());}return Math.min(1,before/(double)Math.max(1,book.getLong("total_chars")));
    }
    private Response bookmarks(SQLiteDatabase db,Request request,String book,String[] parts)throws Exception {
        if(request.method.equals("GET"))return json(object("bookmarks",new JSONArray(records(db,"bookmark",book))));
        if(request.method.equals("POST")){JSONObject input=request.json();int ch=integer(input,"chapter",0),para=integer(input,"para",0),offset=integer(input,"offset",0);validateLocation(db,book,book(db,book),ch,para,offset);
            for(JSONObject old:records(db,"bookmark",book))if(old.getInt("chapter")==ch&&old.getInt("para")==para)return json(object("bookmark",old));
            JSONObject item=new JSONObject().put("id","bm_"+UUID.randomUUID().toString().replace("-","")).put("book_id",book).put("chapter",ch).put("para",para).put("offset",offset).put("label",input.opt("label")).put("created_at",now());saveRecord(db,"bookmark",item.getString("id"),book,item);return new Response(201,object("bookmark",item));}
        if(request.method.equals("DELETE")&&parts.length==6){int changed=db.delete("records","kind=? AND id=? AND book=?",new String[]{"bookmark",parts[5],book});if(changed==0)throw missing();return json(object("ok",true));}throw method();
    }
    private Response annotations(SQLiteDatabase db,Request request,String book,String[] parts)throws Exception {
        if(request.method.equals("GET")){List<JSONObject> all=records(db,"annotation",book);Collections.sort(all,new Comparator<JSONObject>(){@Override public int compare(JSONObject a,JSONObject b){int c=Integer.compare(a.optInt("chapter"),b.optInt("chapter"));if(c==0)c=Integer.compare(a.optInt("para"),b.optInt("para"));if(c==0)c=Integer.compare(a.optInt("start_offset"),b.optInt("start_offset"));return c;}});JSONArray values=new JSONArray();int skip=query(request,"offset",0,0,Integer.MAX_VALUE),limit=query(request,"limit",200,1,500),matched=0;
            for(JSONObject item:all){if(request.query.containsKey("chapter")&&item.getInt("chapter")!=query(request,"chapter",0,0,Integer.MAX_VALUE))continue;if(matched++<skip)continue;if(values.length()>=limit)break;values.put(item);}return json(object("annotations",values));}
        if(request.method.equals("POST")){JSONObject input=request.json();int ch=integer(input,"chapter",0),para=integer(input,"para",0),start=integer(input,"start_offset",0),end=integer(input,"end_offset",0);JSONObject metadata=book(db,book);validateLocation(db,book,metadata,ch,para,end);if(end<=start||metadata.getString("format").equals("cbz"))throw invalid("高亮范围无效");String paragraph=chapter(db,book,ch).getJSONArray("paragraphs").getString(para);String text=paragraph.substring(paragraph.offsetByCodePoints(0,start),paragraph.offsetByCodePoints(0,end));if(!text.equals(input.optString("text")))throw invalid("高亮文字与原文不一致");
            JSONObject item=new JSONObject().put("id","an_"+UUID.randomUUID().toString().replace("-","")).put("book_id",book).put("chapter",ch).put("para",para).put("start_offset",start).put("end_offset",end).put("text",text).put("note",note(input)).put("color",color(input.optString("color","yellow"))).put("created_at",now()).put("updated_at",now());saveRecord(db,"annotation",item.getString("id"),book,item);return new Response(201,object("annotation",item));}
        if(parts.length==6){JSONObject existing=record(db,"annotation",parts[5]);if(existing==null||!book.equals(existing.optString("book_id")))throw missing();
            if(request.method.equals("PATCH")){JSONObject input=request.json();if(input.has("note"))existing.put("note",note(input));if(input.has("color"))existing.put("color",color(input.getString("color")));existing.put("updated_at",now());saveRecord(db,"annotation",parts[5],book,existing);return json(object("annotation",existing));}
            if(request.method.equals("DELETE")){db.delete("records","kind=? AND id=? AND book=?",new String[]{"annotation",parts[5],book});return json(object("ok",true));}}
        throw method();
    }
    private static String color(String color)throws HttpError {if(!Arrays.asList("yellow","green","blue","pink").contains(color))throw invalid("高亮颜色无效");return color;}
    private static Object note(JSONObject input)throws Exception {if(!input.has("note")||input.isNull("note"))return JSONObject.NULL;String value=input.getString("note");if(value.length()>20000)throw invalid("笔记内容过长");return value;}
    private Response search(SQLiteDatabase db,Request request,String id)throws Exception {
        String query=request.query.get("q");if(query==null||query.trim().isEmpty())throw invalid("请输入搜索文字");query=query.trim();if(query.length()>500)throw invalid("搜索文字过长");JSONArray hits=new JSONArray();int limit=query(request,"limit",20,1,100);
        try(Cursor cursor=db.rawQuery("SELECT idx,json FROM chapters WHERE book=? ORDER BY idx",new String[]{id})){while(cursor.moveToNext()&&hits.length()<limit){JSONObject chapter=new JSONObject(cursor.getString(1));JSONArray paragraphs=chapter.getJSONArray("paragraphs");for(int i=0;i<paragraphs.length()&&hits.length()<limit;i++){String text=paragraphs.getString(i);int index=text.indexOf(query);if(index<0)continue;hits.put(new JSONObject().put("chapter",cursor.getInt(0)).put("para",i).put("offset",text.codePointCount(0,index)).put("snippet",escape(text.substring(Math.max(0,index-45),index))+"<b>"+escape(query)+"</b>"+escape(text.substring(index+query.length(),Math.min(text.length(),index+query.length()+65)))));}}}
        return json(new JSONObject().put("book_id",id).put("q",query).put("hits",hits).put("elapsed_ms",0).put("index","local"));
    }
    private Response pages(SQLiteDatabase db,String id,JSONObject book,String[] parts,Request request)throws Exception {
        if(!book.getString("format").equals("cbz"))throw invalid("这本书不是漫画");int from=parts.length==6?number(parts[5],0,2000):query(request,"from",0,0,2000),limit=parts.length==6?1:query(request,"limit",12,1,50);JSONArray pages=new JSONArray();
        try(Cursor cursor=db.rawQuery("SELECT id,width,height FROM media WHERE book=? AND chapter=-1 AND CAST(id AS INTEGER)>=? ORDER BY CAST(id AS INTEGER) LIMIT ?",new String[]{id,String.valueOf(from),String.valueOf(limit)})){while(cursor.moveToNext())pages.put(new JSONObject().put("idx",Integer.parseInt(cursor.getString(0))).put("url","/media/"+id+"/"+cursor.getString(0)).put("width",cursor.getInt(1)).put("height",cursor.getInt(2)));}
        if(parts.length==6){if(pages.length()==0)throw missing();JSONObject page=pages.getJSONObject(0);return json(page.put("book_id",id).put("count",book.getInt("chapter_count")));}
        return json(new JSONObject().put("pages",pages).put("count",book.getInt("chapter_count")).put("from",from));
    }
    private Response media(SQLiteDatabase db,String id,String asset)throws Exception {try(Cursor cursor=db.rawQuery("SELECT type,file FROM media WHERE book=? AND id=?",new String[]{id,asset})){if(!cursor.moveToFirst())throw missing();return file(cursor.getString(0),safeFile(id,cursor.getString(1)));}}
    private Response asset(String path)throws Exception {
        if(path.equals("/"))path="/index.html";
        if(!path.matches("/(index\\.html|app\\.js|sw\\.js|manifest\\.webmanifest|(?:ui|util)/[A-Za-z0-9_-]+\\.js|styles/[A-Za-z0-9_-]+\\.css|icons/[A-Za-z0-9_.-]+\\.(?:png|svg|ico|webp))"))throw missing();
        String type=path.endsWith(".js")?"application/javascript; charset=utf-8":path.endsWith(".css")?"text/css; charset=utf-8":path.endsWith(".html")?"text/html; charset=utf-8":path.endsWith(".webmanifest")?"application/manifest+json":path.endsWith(".png")?"image/png":path.endsWith(".svg")?"image/svg+xml":"application/octet-stream";
        try(InputStream input=context.getAssets().open("web"+path)){ByteArrayOutputStream bytes=new ByteArrayOutputStream();byte[] buffer=new byte[8192];for(int n;(n=input.read(buffer))!=-1;)bytes.write(buffer,0,n);Response response=new Response(new JSONObject());response.type=type;response.bytes=bytes.toByteArray();return response;}catch(FileNotFoundException error){throw missing();}
    }
    private File safeFile(String book,String name)throws IOException {if(!book.matches("b_[a-zA-Z0-9_-]+")||!name.matches("[a-zA-Z0-9_.-]*")||name.equals(".")||name.equals(".."))throw new IOException("文件路径无效");File file=new File(new File(booksRoot,book),name).getCanonicalFile();String prefix=booksRoot.getCanonicalPath()+File.separator;if(!file.getPath().startsWith(prefix))throw new IOException("文件路径越界");return file;}
    private void deleteTree(File file)throws IOException {String root=booksRoot.getCanonicalPath()+File.separator;if(!file.getCanonicalPath().startsWith(root))throw new IOException("清理路径越界");if(file.isDirectory()){File[] children=file.listFiles();if(children!=null)for(File child:children)deleteTree(child);}if(!file.delete()&&file.exists())throw new IOException("无法清理导入临时文件");}
    private static Response file(String type,File file)throws IOException {return new Response(type,new FileInputStream(file),file.length());}
    private static Response json(JSONObject value){return new Response(value);}
    private static int integer(JSONObject object,String key,int fallback)throws Exception {if(!object.has(key))return fallback;Object raw=object.get(key);if(!(raw instanceof Number)||raw instanceof Double&&((Double)raw)%1!=0)throw invalid("位置必须是非负整数");return number(String.valueOf(((Number)raw).longValue()),0,Integer.MAX_VALUE);}
    private static int query(Request request,String key,int fallback,int min,int max)throws HttpError {return request.query.containsKey(key)?number(request.query.get(key),min,max):fallback;}
    private static int number(String raw,int min,int max)throws HttpError {try{int value=Integer.parseInt(raw);if(value<min||value>max)throw invalid("数字超出范围");return value;}catch(NumberFormatException error){throw invalid("数字格式无效");}}
    private static long now(){return System.currentTimeMillis()/1000;}
    private static String escape(String text){return text.replace("&","&amp;").replace("<","&lt;").replace(">","&gt;");}
    private static HttpError missing(){return new HttpError(404,"not_found","资源不存在");}
    private static HttpError method(){return new HttpError(405,"method_not_allowed","请求方法不支持");}
    private static HttpError invalid(String message){return new HttpError(400,"invalid_request",message);}
    private static HttpError unavailable(){return new HttpError(503,"optional_service_required","此功能需要在设置中连接电脑服务；本机阅读和导入可直接使用");}
}
