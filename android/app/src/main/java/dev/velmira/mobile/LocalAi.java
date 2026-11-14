package dev.velmira.mobile;

import android.content.Context;
import android.graphics.BitmapFactory;
import android.util.Base64;
import org.json.JSONObject;
import java.io.*;
import java.net.*;
import java.nio.charset.Charset;
import java.util.*;

/** Bounded mobile HTTP helpers. Provider bodies and credentials never enter errors. */
public final class LocalAi {
    public static final int MAX_IMAGE_BYTES=32*1024*1024;
    private LocalAi(){}
    public static final class NetworkException extends Exception {
        public final int status; public final String code;
        public NetworkException(int status,String code,String message){super(message);this.status=status;this.code=code;}
    }
    public static final class SavedImage {
        public final String id,type; public final File file; public final int width,height;
        SavedImage(String id,String type,File file,int width,int height){this.id=id;this.type=type;this.file=file;this.width=width;this.height=height;}
    }
    /** Caller supplies the complete endpoint, including the configured API prefix. */
    public static JSONObject requestJson(String address,String method,JSONObject body,Map<String,String> headers,int timeout,int maxBytes)throws Exception {
        if(timeout<1000||timeout>300000||maxBytes<1||maxBytes>64*1024*1024)throw invalid("请求限制无效");
        if(!Arrays.asList("GET","POST","PATCH","PUT","DELETE").contains(method))throw invalid("请求方式无效");
        if(headers!=null)for(Map.Entry<String,String> header:headers.entrySet()){
            String key=header.getKey(),value=header.getValue();
            if(key==null||!key.matches("[A-Za-z0-9-]{1,100}")||value==null||value.length()>4000||value.indexOf('\r')>=0||value.indexOf('\n')>=0)throw invalid("请求头格式无效");
        }
        URL url=validateUrl(address,false);HttpURLConnection connection=(HttpURLConnection)url.openConnection();
        connection.setInstanceFollowRedirects(false);connection.setConnectTimeout(Math.min(timeout,20000));connection.setReadTimeout(timeout);
        connection.setRequestMethod(method);connection.setRequestProperty("Accept","application/json");
        if(headers!=null)for(Map.Entry<String,String> header:headers.entrySet()){
            String key=header.getKey(),value=header.getValue();
            connection.setRequestProperty(key,value);
        }
        long deadline=System.currentTimeMillis()+timeout;
        try{
            if(body!=null){
                byte[] bytes=body.toString().getBytes("UTF-8");if(bytes.length>2*1024*1024)throw invalid("请求内容过大");
                connection.setDoOutput(true);connection.setRequestProperty("Content-Type","application/json; charset=utf-8");connection.setFixedLengthStreamingMode(bytes.length);
                try(OutputStream output=connection.getOutputStream()){output.write(bytes);}
            }
            int status=connection.getResponseCode();
            // Never forward Authorization across redirects or expose upstream bodies.
            if(status<200||status>=300)throw new NetworkException(status>=300&&status<400?502:status,"ai_upstream_failed",status>=300&&status<400?"模型服务返回了重定向，请填写最终 API 地址":"模型服务调用失败（HTTP "+status+"），请检查网络和模型配置");
            if(connection.getContentLengthLong()>maxBytes)throw tooLarge();
            try(InputStream input=connection.getInputStream()){
                return new JSONObject(new String(read(input,maxBytes,deadline),Charset.forName("UTF-8")));
            }
        }catch(NetworkException error){throw error;}catch(Exception error){throw new NetworkException(502,"ai_network_failed","无法读取模型服务响应，请检查网络与配置");}finally{connection.disconnect();}
    }
    /** Accepts b64_json, a data:image URI or a signed HTTPS URL. No API auth is sent. */
    public static SavedImage saveImage(Context context,String encodedOrUrl)throws Exception {
        if(encodedOrUrl==null||encodedOrUrl.isEmpty())throw invalid("生图服务没有返回图片");
        byte[] bytes;
        if(encodedOrUrl.startsWith("https://")||encodedOrUrl.startsWith("http://"))bytes=download(encodedOrUrl);
        else{
            String encoded=encodedOrUrl;
            if(encoded.startsWith("data:")){
                int comma=encoded.indexOf(',');if(comma<0||!encoded.substring(0,comma).matches("data:image/(png|jpeg|jpg|webp|gif);base64"))throw invalid("图片编码格式无效");encoded=encoded.substring(comma+1);
            }
            if(encoded.length()>45*1024*1024)throw tooLarge();
            try{bytes=Base64.decode(encoded,Base64.DEFAULT);}catch(IllegalArgumentException error){throw invalid("图片编码格式无效");}
        }
        BitmapFactory.Options image=inspect(bytes);String id="img_"+UUID.randomUUID().toString().replace("-","");
        File directory=new File(context.getFilesDir(),"ai-images");if(!directory.isDirectory()&&!directory.mkdirs())throw new NetworkException(500,"image_write_failed","无法创建手机图片目录");
        File file=new File(directory,id);boolean saved=false;
        try{try(OutputStream output=new FileOutputStream(file)){output.write(bytes);}saved=true;return new SavedImage(id,image.outMimeType,file,image.outWidth,image.outHeight);}
        catch(Exception failed){throw new NetworkException(500,"image_write_failed","图片保存失败，请检查手机存储空间");}
        finally{if(!saved)file.delete();}
    }
    public static File downloadImage(Context context,String encodedOrUrl)throws Exception{return saveImage(context,encodedOrUrl).file;}
    public static String imageType(File file)throws Exception{
        try(InputStream input=new FileInputStream(file)){return inspect(read(input,MAX_IMAGE_BYTES,System.currentTimeMillis()+30000)).outMimeType;}
    }
    public static byte[] readImage(File file)throws Exception{
        try(InputStream input=new FileInputStream(file)){byte[] bytes=read(input,MAX_IMAGE_BYTES,System.currentTimeMillis()+30000);inspect(bytes);return bytes;}
    }
    private static URL validateUrl(String address,boolean media)throws Exception {
        URL url;try{url=new URL(address);}catch(Exception error){throw invalid("服务地址无效");}
        if(!(url.getProtocol().equals("https")||url.getProtocol().equals("http"))||url.getHost().isEmpty()||url.getUserInfo()!=null||url.getRef()!=null||url.getPort()==0||url.getPort()>65535)throw invalid("服务地址必须是有效 HTTP(S) 地址");
        if(media&&url.getProtocol().equals("http")&&!Arrays.asList("localhost","127.0.0.1","[::1]","::1").contains(url.getHost()))throw invalid("远程图片地址必须使用 HTTPS");
        return url;
    }
    private static byte[] download(String address)throws Exception{
        URL url=validateUrl(address,true);HttpURLConnection connection=(HttpURLConnection)url.openConnection();connection.setInstanceFollowRedirects(false);connection.setConnectTimeout(15000);connection.setReadTimeout(90000);
        try{
            if(connection.getResponseCode()!=200)throw new NetworkException(502,"image_download_failed","图片下载失败；请检查生图服务返回的图片地址");
            if(connection.getContentLengthLong()>MAX_IMAGE_BYTES)throw tooLarge();
            try(InputStream input=connection.getInputStream()){return read(input,MAX_IMAGE_BYTES,System.currentTimeMillis()+90000);}
        }catch(NetworkException error){throw error;}catch(Exception error){throw new NetworkException(502,"image_download_failed","图片下载失败，请检查网络");}finally{connection.disconnect();}
    }
    private static BitmapFactory.Options inspect(byte[] bytes)throws Exception{
        if(bytes.length==0||bytes.length>MAX_IMAGE_BYTES)throw tooLarge();BitmapFactory.Options options=new BitmapFactory.Options();options.inJustDecodeBounds=true;BitmapFactory.decodeByteArray(bytes,0,bytes.length,options);
        if(options.outWidth<1||options.outHeight<1||options.outWidth>30000||options.outHeight>30000||(long)options.outWidth*options.outHeight>100000000L||!Arrays.asList("image/png","image/jpeg","image/webp","image/gif").contains(options.outMimeType))throw new NetworkException(502,"image_invalid","生图服务返回了无效或不支持的图片");return options;
    }
    private static byte[] read(InputStream input,int maxBytes,long deadline)throws Exception{
        ByteArrayOutputStream output=new ByteArrayOutputStream();byte[] buffer=new byte[16384];int total=0;
        for(int n;(n=input.read(buffer))!=-1;){if(System.currentTimeMillis()>deadline)throw new NetworkException(504,"ai_timeout","模型响应超时，请稍后重试");total+=n;if(total>maxBytes)throw tooLarge();output.write(buffer,0,n);}return output.toByteArray();
    }
    private static NetworkException invalid(String message){return new NetworkException(400,"ai_request_invalid",message);}
    private static NetworkException tooLarge(){return new NetworkException(502,"ai_response_too_large","模型响应超过允许大小");}
}
