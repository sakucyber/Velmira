# 示例文件（samples/）

| 文件 | 说明 |
| --- | --- |
| demo-comic.cbz | 6 页示例漫画，用来验证/演示 Velmira 的 CBZ 漫画阅读模式（翻页/进度）。内容为程序绘制的占位分镜，**不是小说插图**。 |

用法：浏览器打开 Velmira 书库页，把 demo-comic.cbz 拖进导入区即可；或用 API：

~~~powershell
curl.exe -X POST "http://127.0.0.1:8765/api/books?filename=demo-comic.cbz" --data-binary "@samples/demo-comic.cbz" -H "Content-Type: application/octet-stream"
~~~

自己造更多示例：任何 zip 里放 jpg/png/webp 图片并把扩展名改成 .cbz 即可（文件名顺序即页序，支持 1,2,10 自然排序）。
