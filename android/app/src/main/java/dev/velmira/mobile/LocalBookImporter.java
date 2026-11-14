package dev.velmira.mobile;

import android.graphics.BitmapFactory;
import org.json.JSONArray;
import org.json.JSONObject;
import org.w3c.dom.Document;
import org.w3c.dom.Element;
import org.w3c.dom.Node;
import org.w3c.dom.NodeList;
import org.xml.sax.InputSource;
import org.xml.sax.EntityResolver;
import javax.xml.parsers.DocumentBuilder;
import javax.xml.parsers.DocumentBuilderFactory;
import java.io.*;
import java.net.URLDecoder;
import java.nio.ByteBuffer;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.Charset;
import java.nio.charset.CodingErrorAction;
import java.security.MessageDigest;
import java.util.*;
import java.util.regex.Pattern;
import java.util.zip.ZipEntry;
import java.util.zip.ZipFile;

/** Imports local files into an application-private staging directory. No EPUB
 * markup is executed and no archive path is ever used as a filesystem path. */
final class LocalBookImporter {
    static final long MAX_SOURCE = 64L * 1024 * 1024;
    private static final long MAX_EXPANDED = 512L * 1024 * 1024;
    private static final int MAX_XML_DEPTH = 128;
    private static final Pattern CHAPTER = Pattern.compile("^(第[0-9一二三四五六七八九十百千万零〇两]+[章回节卷部篇].{0,75}|[Cc]hapter\\s+\\d+.{0,70}|序章|楔子|前言|后记|尾声)$");
    private static final Set<String> BLOCKS = new HashSet<>(Arrays.asList("p","div","li","blockquote","pre","section","article","h1","h2","h3","h4","h5","h6","br","tr","figcaption"));
    private static final Set<String> SKIP = new HashSet<>(Arrays.asList("script","style","head","noscript","svg","math","template"));
    static final class Media {
        final String id, type; final File file; final int width, height; final int chapter;
        Media(String id, String type, File file, int width, int height, int chapter) {
            this.id=id; this.type=type; this.file=file; this.width=width; this.height=height; this.chapter=chapter;
        }
    }
    static final class Parsed {
        final JSONObject book; final List<JSONObject> chapters = new ArrayList<>();
        final List<Media> media = new ArrayList<>(); final File directory;
        long expandedBytes;
        Parsed(JSONObject book, File directory) { this.book=book; this.directory=directory; }
    }
    static Parsed parse(File source, String filename, File stage) throws Exception {
        if (source.length() < 1 || source.length() > MAX_SOURCE) throw new IOException("文件为空或超过 64MB");
        if ((!stage.isDirectory() && !stage.mkdirs()) || !stage.isDirectory()) throw new IOException("无法创建书籍存储目录");
        String format;
        byte[] magic=new byte[4]; try (InputStream input=new FileInputStream(source)) { input.read(magic); }
        boolean zip=magic[0]=='P' && magic[1]=='K';
        String name=filename == null ? "未命名.txt" : filename.replace('\\','/'); name=name.substring(name.lastIndexOf('/')+1);
        String lower=name.toLowerCase(Locale.ROOT);
        if (zip) format=lower.endsWith(".epub") ? "epub" : lower.endsWith(".cbz") || lower.endsWith(".zip") ? "cbz" : "epub";
        else if (lower.endsWith(".epub") || lower.endsWith(".cbz")) throw new IOException("文件内容与格式不一致");
        else format="txt";
        String title=name.replaceFirst("(?i)\\.(txt|epub|cbz|zip)$", "");
        JSONObject book=new JSONObject().put("id", "b_"+UUID.randomUUID().toString().replace("-", "").substring(0,16))
            .put("title",title.isEmpty()?"未命名书籍":title).put("author",JSONObject.NULL).put("format",format)
            .put("encoding",format.equals("txt")?"UTF-8":"UTF-8").put("created_at",System.currentTimeMillis()/1000)
            .put("sha256",sha256(source));
        Parsed result=new Parsed(book,stage);
        if (format.equals("txt")) parseText(result,source);
        else try (ZipFile archive=new ZipFile(source)) {
            Map<String,ZipEntry> entries=entries(archive);
            // Content determines ZIP format, including EPUBs selected as .zip.
            boolean epub=entries.containsKey("META-INF/container.xml");
            if (!epub && entries.containsKey("mimetype")) epub=new String(zipRead(result,archive,entries,"mimetype",256),Charset.forName("UTF-8")).trim().equals("application/epub+zip");
            if (!epub) for(String path:entries.keySet()) if(path.toLowerCase(Locale.ROOT).endsWith(".opf")) { epub=true;break; }
            format=epub?"epub":"cbz"; result.book.put("format",format);
            if (format.equals("epub")) parseEpub(result,archive,entries);
            else parseComic(result,archive,entries);
        }
        long chars=0; int paragraphs=0;
        for (JSONObject chapter:result.chapters) { chars+=chapter.getLong("chars"); paragraphs+=chapter.getJSONArray("paragraphs").length(); }
        int count=format.equals("cbz")?result.media.size():result.chapters.size();
        if(count==0) throw new IOException("书籍没有可阅读的正文或图片");
        result.book.put("total_chars",chars).put("chapter_count",count).put("para_count",paragraphs);
        if(format.equals("cbz")) result.book.put("page_count",count);
        return result;
    }
    static String sha256(File file) throws Exception {
        MessageDigest digest=MessageDigest.getInstance("SHA-256"); byte[] buffer=new byte[16384];
        try(InputStream input=new FileInputStream(file)) { for(int n;(n=input.read(buffer))!=-1;) digest.update(buffer,0,n); }
        StringBuilder hex=new StringBuilder(); for(byte value:digest.digest()) hex.append(String.format(Locale.ROOT,"%02x",value & 255)); return hex.toString();
    }
    private static byte[] read(File file,long limit) throws IOException { try(InputStream input=new FileInputStream(file)) { return read(input,limit); } }
    private static byte[] read(InputStream input,long limit) throws IOException {
        ByteArrayOutputStream output=new ByteArrayOutputStream(); byte[] buffer=new byte[8192]; long total=0;
        for(int n;(n=input.read(buffer))!=-1;) { total+=n; if(total>limit) throw new IOException("解压内容超过限制"); output.write(buffer,0,n); }
        return output.toByteArray();
    }
    private static String decode(byte[] data) throws IOException {
        if(data.length>=2 && data[0]==(byte)0xff && data[1]==(byte)0xfe) return new String(data,2,data.length-2,Charset.forName("UTF-16LE"));
        if(data.length>=2 && data[0]==(byte)0xfe && data[1]==(byte)0xff) return new String(data,2,data.length-2,Charset.forName("UTF-16BE"));
        try { return Charset.forName("UTF-8").newDecoder().onMalformedInput(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(data)).toString().replace("\ufeff",""); }
        catch(CharacterCodingException error) {
            try { return Charset.forName("GB18030").newDecoder().onMalformedInput(CodingErrorAction.REPORT).onUnmappableCharacter(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(data)).toString(); }
            catch(CharacterCodingException invalid) { throw new IOException("文本编码无效"); }
        }
    }
    private static void parseText(Parsed result,File source) throws Exception {
        byte[] prefix=new byte[4]; int n;
        try(InputStream input=new FileInputStream(source)) { n=input.read(prefix); }
        String encoding="UTF-8"; int skip=0;
        if(n>=2 && prefix[0]==(byte)0xff && prefix[1]==(byte)0xfe) {encoding="UTF-16LE";skip=2;}
        else if(n>=2 && prefix[0]==(byte)0xfe && prefix[1]==(byte)0xff) {encoding="UTF-16BE";skip=2;}
        else if(n>=3 && prefix[0]==(byte)0xef && prefix[1]==(byte)0xbb && prefix[2]==(byte)0xbf) skip=3;
        else if(n==4 && prefix[1]==0 && prefix[3]==0 && prefix[0]!=0 && prefix[2]!=0) encoding="UTF-16LE";
        else if(n==4 && prefix[0]==0 && prefix[2]==0 && prefix[1]!=0 && prefix[3]!=0) encoding="UTF-16BE";
        else {
            // Probe UTF-8 with a bounded character buffer; a large novel never
            // needs a second full byte[] and String copy just for detection.
            try(Reader reader=textReader(source,"UTF-8",0)) {char[] buffer=new char[8192];while(reader.read(buffer)!=-1){} }
            catch(CharacterCodingException invalid) {encoding="GB18030";}
        }
        result.book.put("encoding",encoding);
        TextChapters text=new TextChapters(result); StringBuilder line=new StringBuilder();
        try(Reader reader=textReader(source,encoding,skip)) {
            char[] buffer=new char[8192];
            for(int count;(count=reader.read(buffer))!=-1;) for(int i=0;i<count;i++) {
                char c=buffer[i];
                if(c==0) throw new IOException("不支持的文本编码或二进制文件");
                if(c=='\n' || c=='\r') {text.line(line.toString());line.setLength(0);}
                else {
                    if(c!='\ufeff' && c!='\u200b')line.append(c);
                    if(line.length()>=8192 && !Character.isHighSurrogate(c)) {text.line(line.toString());line.setLength(0);}
                }
            }
        }
        text.line(line.toString());text.finish();
    }
    private static Reader textReader(File source,String encoding,int skip) throws IOException {
        InputStream input=new FileInputStream(source);
        try {
            for(int i=0;i<skip;i++) if(input.read()<0)break;
            return new BufferedReader(new InputStreamReader(input,Charset.forName(encoding).newDecoder().onMalformedInput(CodingErrorAction.REPORT).onUnmappableCharacter(CodingErrorAction.REPORT)),8192);
        } catch(Exception failure) {input.close();throw failure;}
    }
    private static final class TextChapters {
        final Parsed result; JSONArray paragraphs=new JSONArray();String title="正文";
        TextChapters(Parsed result) {this.result=result;}
        void line(String value) throws Exception {
            value=value.trim();if(value.isEmpty())return;
            if(CHAPTER.matcher(value).matches()) {finish();title=value;}
            else paragraphs.put(value);
        }
        void finish() throws Exception {if(paragraphs.length()>0)addChapter(result,title,paragraphs,new JSONArray());paragraphs=new JSONArray();}
    }
    private static void addChapter(Parsed result,String title,JSONArray paragraphs,JSONArray images) throws Exception {
        long chars=0; for(int i=0;i<paragraphs.length();i++) { String value=paragraphs.getString(i); chars+=value.codePointCount(0,value.length()); }
        int index=result.chapters.size(); result.chapters.add(new JSONObject().put("book_id",result.book.getString("id")).put("idx",index)
            .put("title",title).put("chars",chars).put("paragraphs",paragraphs).put("illustrations",images).put("paragraph_formats",new JSONArray()));
    }
    private static Map<String,ZipEntry> entries(ZipFile zip) throws Exception {
        Map<String,ZipEntry> result=new LinkedHashMap<>(); long total=0;int entryCount=0;
        for(Enumeration<? extends ZipEntry> all=zip.entries();all.hasMoreElements();) {
            if(++entryCount>10000)throw new IOException("压缩包条目过多");
            ZipEntry entry=all.nextElement(); safePath(entry.getName()); if(entry.isDirectory()) continue;
            if(result.size()>=10000) throw new IOException("压缩包条目过多");
            String name=entry.getName(); safePath(name);
            if(result.put(name,entry)!=null) throw new IOException("压缩包含重复路径");
            if(entry.getSize()>MAX_EXPANDED || entry.getSize() < -1) throw new IOException("解压内容超过限制");
            total+=Math.max(0,entry.getSize()); if(total>MAX_EXPANDED) throw new IOException("解压内容超过 512MB");
        }
        return result;
    }
    static String safePath(String value) throws IOException {
        if(value==null || value.isEmpty() || value.length()>2048 || value.startsWith("/") || value.indexOf('\\')>=0 || value.indexOf(':')>=0 || value.indexOf('\0')>=0 || value.contains("//")) throw new IOException("压缩包路径无效");
        for(String part:value.split("/")) if(part.equals("..") || part.equals(".")) throw new IOException("压缩包路径无效");
        return value;
    }
    private static String resolve(String base,String href) throws Exception {
        if(href==null || href.isEmpty()) return "";
        href=URLDecoder.decode(href.split("#",2)[0].replace("+","%2B"),"UTF-8");
        if(href.startsWith("/") || href.indexOf(':')>=0 || href.indexOf('\\')>=0 || href.indexOf('?')>=0) throw new IOException("EPUB 仅支持包内资源");
        List<String> parts=new ArrayList<>(); String joined=base+href;
        for(String part:joined.split("/")) {
            if(part.isEmpty() || part.equals(".")) continue;
            if(part.equals("..")) { if(parts.isEmpty()) throw new IOException("EPUB 资源越界"); parts.remove(parts.size()-1); }
            else parts.add(part);
        }
        return safePath(join(parts,"/"));
    }
    private static String join(List<String> parts,String sep) { StringBuilder value=new StringBuilder(); for(String part:parts) { if(value.length()>0)value.append(sep);value.append(part); } return value.toString(); }
    private static String parent(String name) { int slash=name.lastIndexOf('/'); return slash<0?"":name.substring(0,slash+1); }
    private static byte[] zipRead(Parsed result,ZipFile zip,Map<String,ZipEntry> entries,String path,long limit) throws Exception {
        ZipEntry entry=entries.get(path); if(entry==null) throw new IOException("EPUB 资源缺失");
        try(InputStream input=zip.getInputStream(entry)) {
            byte[] value=read(input,Math.min(limit,MAX_EXPANDED-result.expandedBytes));result.expandedBytes+=value.length;return value;
        }
    }
    private static Document xml(byte[] data) throws Exception {
        String text=decode(data);
        if(text.toUpperCase(Locale.ROOT).contains("<!ENTITY")) throw new IOException("XML 实体不受支持");
        text=text.replaceAll("(?is)<!DOCTYPE[^>]*>","");
        String[][] entities={{"nbsp","160"},{"hellip","8230"},{"mdash","8212"},{"ndash","8211"},{"ldquo","8220"},{"rdquo","8221"},{"lsquo","8216"},{"rsquo","8217"},{"copy","169"},{"reg","174"}};
        for(String[] entity:entities)text=text.replace("&"+entity[0]+";","&#"+entity[1]+";");
        DocumentBuilderFactory factory=DocumentBuilderFactory.newInstance(); factory.setNamespaceAware(true); factory.setExpandEntityReferences(false);
        try {factory.setFeature("http://xml.org/sax/features/external-general-entities",false);}catch(Exception ignored){}
        try {factory.setFeature("http://xml.org/sax/features/external-parameter-entities",false);}catch(Exception ignored){}
        try {factory.setFeature("http://apache.org/xml/features/nonvalidating/load-external-dtd",false);}catch(Exception ignored){}
        DocumentBuilder parser=factory.newDocumentBuilder(); parser.setEntityResolver(new EntityResolver() {
            @Override public InputSource resolveEntity(String publicId,String systemId) {return new InputSource(new StringReader(""));}
        });
        Document document=parser.parse(new InputSource(new StringReader(text)));
        checkTree(document.getDocumentElement(),1,new int[1]);return document;
    }
    private static void checkTree(Node node,int depth,int[] count) throws IOException {
        if(depth>MAX_XML_DEPTH || ++count[0]>200000)throw new IOException("EPUB 文档结构过于复杂");
        for(Node child=node.getFirstChild();child!=null;child=child.getNextSibling())checkTree(child,depth+1,count);
    }
    private static String local(Node node) { String value=node.getLocalName(); if(value==null)value=node.getNodeName(); return value.toLowerCase(Locale.ROOT); }
    private static List<Element> elements(Document document,String tag) {
        List<Element> result=new ArrayList<>(); NodeList all=document.getElementsByTagName("*");
        for(int i=0;i<all.getLength();i++) if(local(all.item(i)).equals(tag)) result.add((Element)all.item(i)); return result;
    }
    private static void parseEpub(Parsed result,ZipFile zip,Map<String,ZipEntry> entries) throws Exception {
        String opf="";
        if(entries.containsKey("META-INF/container.xml")) {
            for(Element node:elements(xml(zipRead(result,zip,entries,"META-INF/container.xml",1024*1024)),"rootfile")) {
                String fullPath=node.getAttribute("full-path");
                try { opf=safePath(URLDecoder.decode(fullPath.replace("+","%2B"),"UTF-8")); } catch (Exception invalid) { throw new IOException("EPUB OPF 路径无效"); }
                break;
            }
        }
        if(opf.isEmpty()) for(String path:entries.keySet()) if(path.toLowerCase(Locale.ROOT).endsWith(".opf")){opf=path;break;}
        if(opf.isEmpty()) throw new IOException("EPUB 缺少 OPF 文件");
        Document packageDoc=xml(zipRead(result,zip,entries,opf,8*1024*1024));
        for(Element node:elements(packageDoc,"title")) { String value=node.getTextContent().trim(); if(!value.isEmpty())result.book.put("title",value.substring(0,Math.min(200,value.length()))); break; }
        for(Element node:elements(packageDoc,"creator")) { String value=node.getTextContent().trim();result.book.put("author",value.substring(0,Math.min(200,value.length()))); break; }
        Map<String,String> manifest=new HashMap<>();
        for(Element item:elements(packageDoc,"item")) {
            try {manifest.put(item.getAttribute("id"),resolve(parent(opf),item.getAttribute("href")));}
            catch(IOException invalid) { /* External manifest resources are never fetched. */ }
        }
        long imageBytes=0;
        for(Element item:elements(packageDoc,"itemref")) {
            String chapterPath=manifest.get(item.getAttribute("idref")); if(chapterPath==null) continue;
            Document chapter=xml(zipRead(result,zip,entries,chapterPath,8*1024*1024));
            JSONArray paragraphs=new JSONArray(),images=new JSONArray(); StringBuilder current=new StringBuilder();
            Map<Node,Integer> anchors=new HashMap<>();
            walk(chapter.getDocumentElement(),current,paragraphs,anchors); flush(current,paragraphs);
            String title="第 "+(result.chapters.size()+1)+" 部分";
            for(Element node:elements(chapter,"h1")) { title=node.getTextContent().trim();break; }
            if(title.startsWith("第 ")) for(Element node:elements(chapter,"title")) { String t=node.getTextContent().trim(); if(!t.isEmpty())title=t;break; }
            for(Element image:elements(chapter,"img")) {
                String src=image.getAttribute("src"); if(src.isEmpty())continue;
                String path;
                try { path=resolve(parent(chapterPath),src); }catch(IOException error){continue;}
                String type=mime(path); if(type==null || !entries.containsKey(path))continue;
                String id="a_"+result.media.size(); File destination=new File(result.directory,"image_"+result.media.size());
                long bytes=copyMedia(result,zip.getInputStream(entries.get(path)),destination); imageBytes+=bytes;
                if(imageBytes>128L*1024*1024)throw new IOException("EPUB 插图超过 128MB");
                int[] size=dimensions(destination);type=imageType(destination);
                result.media.add(new Media(id,type,destination,size[0],size[1],result.chapters.size()));
                images.put(new JSONObject().put("id",id).put("media_type",type).put("url","/api/books/"+result.book.getString("id")+"/epub/assets/"+id)
                    .put("before_para",anchors.containsKey(image)?anchors.get(image):paragraphs.length()).put("alt",image.getAttribute("alt")));
            }
            if(paragraphs.length()>0 || images.length()>0) addChapter(result,title,paragraphs,images);
        }
    }
    private static void walk(Node node,StringBuilder current,JSONArray paragraphs,Map<Node,Integer> anchors) {
        if(node.getNodeType()==Node.TEXT_NODE || node.getNodeType()==Node.CDATA_SECTION_NODE) {current.append(node.getNodeValue());return;}
        String tag=local(node); if(SKIP.contains(tag))return;
        if(tag.equals("img")) {
            String pending=current.toString().replace('\u00a0',' ').replaceAll("\\s+"," ").trim();
            anchors.put(node,paragraphs.length()+(pending.isEmpty()?0:1));
        }
        boolean block=BLOCKS.contains(tag); if(block)flush(current,paragraphs);
        for(Node child=node.getFirstChild();child!=null;child=child.getNextSibling())walk(child,current,paragraphs,anchors);
        if(block)flush(current,paragraphs);
    }
    private static void flush(StringBuilder current,JSONArray paragraphs) {
        String value=current.toString().replace('\u00a0',' ').replaceAll("\\s+"," ").trim();current.setLength(0);
        if(!value.isEmpty())paragraphs.put(value);
    }
    private static void parseComic(Parsed result,ZipFile zip,Map<String,ZipEntry> entries) throws Exception {
        List<String> paths=new ArrayList<>();for(String path:entries.keySet())if(mime(path)!=null && !path.startsWith("__MACOSX/") && !path.startsWith(".") && !path.contains("/."))paths.add(path);
        Collections.sort(paths,new Comparator<String>() {
            @Override public int compare(String a,String b) {int compare=natural(a.substring(a.lastIndexOf('/')+1),b.substring(b.lastIndexOf('/')+1));return compare==0?natural(a,b):compare;}
        }); if(paths.size()>2000)throw new IOException("漫画超过 2000 页");long total=0;
        for(String path:paths) {
            File file=new File(result.directory,"image_"+result.media.size());total+=copyMedia(result,zip.getInputStream(entries.get(path)),file);
            if(total>MAX_EXPANDED)throw new IOException("漫画图片超过 512MB");int[] size=dimensions(file);
            result.media.add(new Media(String.valueOf(result.media.size()),imageType(file),file,size[0],size[1],-1));
        }
    }
    static int natural(String a,String b) {
        int i=0,j=0; while(i<a.length()&&j<b.length()) {
            char ca=Character.toLowerCase(a.charAt(i)),cb=Character.toLowerCase(b.charAt(j));
            if(Character.isDigit(ca)&&Character.isDigit(cb)) {
                int ai=i,bj=j;while(i<a.length()&&Character.isDigit(a.charAt(i)))i++;while(j<b.length()&&Character.isDigit(b.charAt(j)))j++;
                String an=a.substring(ai,i).replaceFirst("^0+(?!$)",""),bn=b.substring(bj,j).replaceFirst("^0+(?!$)","");
                if(an.length()!=bn.length())return an.length()-bn.length();int cmp=an.compareTo(bn);if(cmp!=0)return cmp;
            } else {if(ca!=cb)return ca-cb;i++;j++;}
        } return a.length()-b.length();
    }
    private static int[] dimensions(File file) throws IOException {
        BitmapFactory.Options options=new BitmapFactory.Options();options.inJustDecodeBounds=true;BitmapFactory.decodeFile(file.getPath(),options);
        if(options.outWidth<1 || options.outHeight<1 || options.outWidth>30000 || options.outHeight>30000 || (long)options.outWidth*options.outHeight>100000000L)throw new IOException("图片无效或尺寸过大");
        return new int[]{options.outWidth,options.outHeight};
    }
    private static long copyMedia(Parsed result,InputStream source,File target) throws IOException {
        long count=copy(source,target,Math.min(32L*1024*1024,MAX_EXPANDED-result.expandedBytes));result.expandedBytes+=count;return count;
    }
    private static String imageType(File file) throws IOException {
        byte[] prefix=new byte[12];int count;try(InputStream input=new FileInputStream(file)){count=input.read(prefix);}
        if(count>=8 && prefix[0]==(byte)0x89 && prefix[1]=='P' && prefix[2]=='N' && prefix[3]=='G' && prefix[4]==13 && prefix[5]==10 && prefix[6]==26 && prefix[7]==10)return "image/png";
        if(count>=3 && prefix[0]==(byte)0xff && prefix[1]==(byte)0xd8 && prefix[2]==(byte)0xff)return "image/jpeg";
        if(count>=6 && prefix[0]=='G' && prefix[1]=='I' && prefix[2]=='F' && prefix[3]=='8' && (prefix[4]=='7'||prefix[4]=='9') && prefix[5]=='a')return "image/gif";
        if(count>=12 && prefix[0]=='R' && prefix[1]=='I' && prefix[2]=='F' && prefix[3]=='F' && prefix[8]=='W' && prefix[9]=='E' && prefix[10]=='B' && prefix[11]=='P')return "image/webp";
        throw new IOException("不支持的图片内容");
    }
    static String mime(String path) {
        String value=path.toLowerCase(Locale.ROOT);if(value.endsWith(".jpg")||value.endsWith(".jpeg"))return "image/jpeg";
        if(value.endsWith(".png"))return "image/png";if(value.endsWith(".webp"))return "image/webp";if(value.endsWith(".gif"))return "image/gif";return null;
    }
    static long copy(InputStream input,File target,long limit) throws IOException {
        try(InputStream source=input;OutputStream output=new FileOutputStream(target)) {byte[] buffer=new byte[16384];long total=0;for(int n;(n=source.read(buffer))!=-1;){total+=n;if(total>limit)throw new IOException("文件超过大小限制");output.write(buffer,0,n);}return total;}
    }
}
