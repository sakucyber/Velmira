//! Compare layout and page raster times without a graphics driver or window.

use std::error::Error;
use std::hint::black_box;
use std::path::Path;
use std::time::{Duration, Instant};
use vm_text::{PageBox, Paginator};

const PARAGRAPHS: &[&str] = &[
    "石板路沿着河岸向东，穿过镇子最后一座拱桥就没进了芦苇荡。昨夜的雨还积在路边，挑菜的人把扁担换到另一侧肩上。门外的瓦片偶尔滴下一颗水珠。",
    "钟响时船正靠岸。掌舵人收起竹篙，从布包里取出写有姓名的册子。码头静了一会儿，随后又有车轮碾过木板的声音。远处厨房的窗户亮了。",
    "她翻开一本夹着干桂花的记事本。三年前到码头问路的人只留下半张纸条，其余的事情全写在下一页。纸沿已被水浸软，落笔处有一个指尖大小的缺口。",
    "午后风起，井口的绳子轻轻碰着桶沿。街边的摊位本该等到天黑才收，今天却提前空了。卖糖的人借了两块木板，把受潮的垫布搬到墙边晾晒。",
    "入冬以后，山口的一段路结了冰。护林人每天出门都要绕过松树林，数清两道石门间落下的树枝。有人在桥上留了一盏灯，值夜的人每天替它添油。",
    "铺子里传出一阵很轻的敲击声，铁匠正把一根短钉从砧上拿起来。他几天前接到一张新图纸，上面只有尺寸，既没有图名，也没有托付人留下的地址。",
    "人群散去后，院里的木凳还围成一圈。新来的孩子把半块芝麻饼放在窗台上，蹲下帮老人捡起一截线头。晚饭过后他们一起把长桌抬到后门旁边。",
    "桌上三张地图的比例各不相同。县城也不在相同的方向，绘图者用细红线标出一条废弃的铁路。书生取来尺子，对着旧车票把两个站之间的距离算了一遍。",
    "离岸的渡船走得很慢，挪过水草才驶上主河道。岸上的人已经认不出船头那件浅色围裙，只有远处的鸟群还跟着走。斜阳正落在堤上的老柳树后面。",
    "隔街的作坊正在给一批纸伞描边。有人给新买的桐油盖紧瓶口，有人打量雨后堆起来的空木箱。下工前，他们在账本末尾补上被折断的两根竹骨。",
];
const VIEWPORT: PageBox = PageBox {
    width: 960.0,
    height: 700.0,
    font_size: 19.0,
    line_height: 28.0,
};
const INK: [u8; 4] = [31, 41, 48, 255];
const REPEATS: usize = 20;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let explicit_font = args.next();
    if args.next().is_some() {
        return Err("usage: bench_layout [font.ttf]".into());
    }
    let paragraphs = chapter();
    let start = Instant::now();
    let mut engine = match explicit_font {
        Some(path) => Paginator::from_font_data(std::fs::read(Path::new(&path))?),
        None => Paginator::new(),
    };
    let font_discovery = start.elapsed();
    let start = Instant::now();
    let layout = engine.paginate(0, &paragraphs, VIEWPORT)?;
    let layout_cold = start.elapsed();
    if layout.missing_glyphs() != 0 {
        return Err(format!(
            "{} unresolved glyphs. Install a Chinese font or pass an explicit font file.",
            layout.missing_glyphs()
        )
        .into());
    }
    let start = Instant::now();
    black_box(engine.rasterize_page(&layout, 0, INK)?);
    let raster_cold = start.elapsed();
    let next_page_raster = if layout.page_count() > 1 {
        let start = Instant::now();
        black_box(engine.rasterize_page(&layout, 1, INK)?);
        Some(start.elapsed())
    } else {
        None
    };

    let mut layout_resize = Vec::with_capacity(REPEATS);
    let mut raster_warm = Vec::with_capacity(REPEATS);
    for n in 0..REPEATS {
        let view = PageBox {
            width: if n % 2 == 0 { 896.0 } else { VIEWPORT.width },
            ..VIEWPORT
        };
        let started = Instant::now();
        black_box(engine.paginate(0, &paragraphs, view)?);
        layout_resize.push(started.elapsed());
        let started = Instant::now();
        black_box(engine.rasterize_page(&layout, 0, INK)?);
        raster_warm.push(started.elapsed());
    }
    println!(
        "chapter=3000 characters, {} paragraphs, {} pages, viewport=960x700, font=19px",
        paragraphs.len(),
        layout.page_count()
    );
    print_ms("font_discovery", font_discovery);
    print_ms("chapter_layout_cold", layout_cold);
    percentiles("chapter_resize_warm", layout_resize);
    print_ms("first_page_raster_cold", raster_cold);
    if let Some(duration) = next_page_raster {
        print_ms("second_page_raster_first_access", duration);
    }
    percentiles("first_page_raster_warm", raster_warm);
    Ok(())
}

fn chapter() -> Vec<String> {
    let mut left = 3000;
    PARAGRAPHS
        .iter()
        .cycle()
        .map_while(|paragraph| {
            if left == 0 {
                return None;
            }
            let count = left.min(paragraph.chars().count());
            left -= count;
            Some(paragraph.chars().take(count).collect())
        })
        .collect()
}

fn percentiles(label: &str, mut samples: Vec<Duration>) {
    samples.sort_unstable();
    print_ms(&format!("{label}_p50"), samples[samples.len() / 2]);
    let n = (samples.len() * 95).div_ceil(100);
    print_ms(&format!("{label}_p95"), samples[n - 1]);
}

fn print_ms(label: &str, duration: Duration) {
    println!("{label}={:.3} ms", duration.as_secs_f64() * 1000.0);
}
