package dev.velmira.mobile;

import java.io.*;
import java.nio.charset.Charset;
import java.util.Arrays;
import java.util.zip.ZipEntry;
import java.util.zip.ZipOutputStream;

/** Runs with Android app_process in an owned temporary directory. */
public final class ImporterProbe {
    private static File root;
    private static int sequence;
    private static byte[] png;

    private static void check(boolean value,String message) {
        if(!value)throw new AssertionError(message);
    }
    private static File file(String extension,byte[] bytes) throws Exception {
        File result=new File(root,"source-"+(++sequence)+extension);
        try(OutputStream output=new FileOutputStream(result)){output.write(bytes);}
        return result;
    }
    private static LocalBookImporter.Parsed parse(File source,String name) throws Exception {
        return LocalBookImporter.parse(source,name,new File(root,"stage-"+(++sequence)));
    }
    private static void entry(ZipOutputStream zip,String name,byte[] value) throws Exception {
        zip.putNextEntry(new ZipEntry(name));zip.write(value);zip.closeEntry();
    }
    private static void textEntry(ZipOutputStream zip,String name,String value) throws Exception {
        entry(zip,name,value.getBytes("UTF-8"));
    }
    private static File epub(String content) throws Exception {
        File source=new File(root,"source-"+(++sequence)+".zip");
        try(ZipOutputStream zip=new ZipOutputStream(new FileOutputStream(source))) {
            textEntry(zip,"mimetype","application/epub+zip");
            textEntry(zip,"META-INF/container.xml","<container><rootfiles><rootfile full-path=\"OEBPS/book.opf\"/></rootfiles></container>");
            textEntry(zip,"OEBPS/book.opf","<package xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><metadata><dc:title>本机插图</dc:title><dc:creator>作者</dc:creator></metadata><manifest><item id=\"one\" href=\"one.xhtml\"/><item id=\"external\" href=\"https://invalid.example/ignored.png\"/></manifest><spine><itemref idref=\"one\"/></spine></package>");
            textEntry(zip,"OEBPS/one.xhtml",content);
            entry(zip,"OEBPS/images/local.png",png);
        }
        return source;
    }
    private static void rejected(File source,String name,String label) throws Exception {
        boolean rejected=false;
        try{parse(source,name);}catch(Exception expected){rejected=true;}
        check(rejected,label);
    }
    public static void main(String[] args) throws Exception {
        root=new File(args[0]);check(root.isDirectory() || root.mkdirs(),"Temporary directory exists");
        try(InputStream input=new FileInputStream(args[1]);ByteArrayOutputStream output=new ByteArrayOutputStream()) {
            byte[] buffer=new byte[4096];for(int count;(count=input.read(buffer))!=-1;)output.write(buffer,0,count);png=output.toByteArray();
        }
        String text="第一章 开始\n\n甲😀乙。\n\n第二章 继续\n\n另一段。\n";
        LocalBookImporter.Parsed utf8=parse(file(".txt",text.getBytes("UTF-8")),"UTF8.txt");
        check(utf8.chapters.size()==2,"UTF-8 chapters");
        check(utf8.chapters.get(0).getLong("chars")==4,"Unicode character offsets count emoji once");
        LocalBookImporter.Parsed gb=parse(file(".txt","第一章 开始\n正文内容。".getBytes("GB18030")),"GB.txt");
        check(gb.book.getString("encoding").equals("GB18030"),"GB18030 detection");
        check(gb.chapters.get(0).getJSONArray("paragraphs").getString(0).equals("正文内容。"),"GB18030 content");
        LocalBookImporter.Parsed utf16=parse(file(".txt",text.getBytes("UTF-16")),"UTF16.txt");
        check(utf16.chapters.size()==2,"UTF-16 BOM");
        rejected(file(".txt",new byte[]{0,1,0,1,0,0}),"binary.txt","Binary text rejects");
        System.out.println("[PASS] Android TXT UTF-8/GB18030/UTF-16, chapter splitting and Unicode coordinates");

        LocalBookImporter.Parsed book=parse(epub("<html><head><title>第一部分</title></head><body><h1>第一章 温室</h1><p>甲&amp;乙&nbsp;正文。</p><script>必须丢弃</script><img src=\"images/local.png\"/><img src=\"https://invalid.example/never.png\"/></body></html>"),"renamed.zip");
        check(book.book.getString("format").equals("epub"),"ZIP content detection");
        check(book.book.getString("title").equals("本机插图"),"EPUB metadata");
        check(book.chapters.size()==1 && book.media.size()==1,"EPUB chapters and local images");
        check(book.media.get(0).width==192 && book.media.get(0).height==192,"Image dimensions decoded");
        check(book.chapters.get(0).toString().indexOf("必须丢弃")==-1,"Scripts omitted");
        check(book.chapters.get(0).getJSONArray("illustrations").length()==1,"External image ignored");
        rejected(epub("<!DOCTYPE html [<!ENTITY xxe SYSTEM 'file:///system/build.prop'>]><html><body><p>&xxe;</p></body></html>"),"bad.epub","XXE rejected");
        StringBuilder deep=new StringBuilder("<html><body>");for(int i=0;i<140;i++)deep.append("<div>");deep.append("正文");for(int i=0;i<140;i++)deep.append("</div>");deep.append("</body></html>");
        rejected(epub(deep.toString()),"deep.epub","Deep XML rejected");
        System.out.println("[PASS] Android EPUB metadata, renamed ZIP detection, local illustration and hostile XML limits");

        File comic=new File(root,"comic.cbz");
        try(ZipOutputStream zip=new ZipOutputStream(new FileOutputStream(comic))) {
            for(int index:new int[]{10,2,1}) {byte[] image=Arrays.copyOf(png,png.length+1);image[image.length-1]=(byte)index;entry(zip,"pages/"+index+".png",image);}
        }
        LocalBookImporter.Parsed pages=parse(comic,"comic.cbz");
        check(pages.media.size()==3 && pages.book.getInt("page_count")==3,"CBZ pages");
        for(int index=0;index<3;index++) {
            try(RandomAccessFile media=new RandomAccessFile(pages.media.get(index).file,"r")) {media.seek(media.length()-1);check(media.read()==new int[]{1,2,10}[index],"Natural page order");}
        }
        File unsafe=new File(root,"unsafe.cbz");try(ZipOutputStream zip=new ZipOutputStream(new FileOutputStream(unsafe))){entry(zip,"../escape.png",png);}
        rejected(unsafe,"unsafe.cbz","Archive traversal rejected");
        File tooLarge=new File(root,"oversized.cbz");
        try(ZipOutputStream zip=new ZipOutputStream(new FileOutputStream(tooLarge))){zip.putNextEntry(new ZipEntry("page.png"));zip.write(png);byte[] zeros=new byte[1024*1024];for(int i=0;i<33;i++)zip.write(zeros);zip.closeEntry();}
        rejected(tooLarge,"oversized.cbz","Actual image expansion limited");
        System.out.println("[PASS] Android CBZ decoding, natural sort, traversal and expansion bounds");
    }
}
