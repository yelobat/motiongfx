use std::fmt;
use std::sync::OnceLock;

use peniko::Color;
use peniko::kurbo::{Affine, BezPath, Rect, Shape as _};
use typst::layout::{Frame, FrameItem, PagedDocument};
use typst::text::Font;
use typst::visualize::{Geometry, Paint};
use typst_as_lib::TypstEngine;

pub use peniko;

/// A compiled math formula.
#[derive(Debug, Clone)]
pub struct MathFormula {
    /// One per glyph.
    pub paths: Vec<FormulaPath>,
    /// Size of the formula in pt.
    pub size: (f64, f64),
}

impl MathFormula {
    /// The union bounding box of all paths.
    pub fn bounds(&self) -> Rect {
        self.paths
            .iter()
            .map(|p| p.path.bounding_box())
            .reduce(|a, b| a.union(b))
            .unwrap_or(Rect::ZERO)
    }
}

/// One outline of a [`MathFormula`].
#[derive(Debug, Clone)]
pub struct FormulaPath {
    /// The outline in pt.
    pub path: BezPath,
    /// Fill color.
    pub fill: Option<Color>,
    pub stroke: Option<(Color, f64)>,
    /// Identity key for morphing.
    pub key: u64,
}

/// Why a compile failed.
#[derive(Debug, Clone)]
pub enum CompileError {
    /// Typst error.
    Typst(String),
    /// LaTeX error.
    LaTeX(String),
    /// Document had no page.
    NoPage,
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Typst(message) => {
                write!(f, "typst: {message}")
            }
            Self::LaTeX(message) => {
                write!(f, "latex: {message}")
            }
            Self::NoPage => write!(f, "document has no page"),
        }
    }
}

impl std::error::Error for CompileError {}

pub fn compile_math(
    source: &str,
    size_pt: f64,
) -> Result<MathFormula, CompileError> {
    let full = format!(
        "#set page(width: auto, height: auto, margin: 0pt, \
         fill: none)\n\
         #set text(size: {size_pt}pt, fill: white)\n\
         $ {source} $\n",
    );
    compile_source(&full)
}

pub fn compile_text(
    source: &str,
    size_pt: f64,
) -> Result<MathFormula, CompileError> {
    let full = format!(
        "#set page(width: auto, height: auto, margin: 0pt, \
         fill: none)\n\
         #set text(size: {size_pt}pt, fill: white)\n\
         {source}\n",
    );
    compile_source(&full)
}

/// The MiTeX Typst-side prelude.
const MITEX_SCOPE: &str = include_str!("../assets/mitex-scope.typ");

/// Compile a LaTeX math source.
pub fn compile_latex_math(
    source: &str,
    size_pt: f64,
) -> Result<MathFormula, CompileError> {
    let converted = latex_to_typst_math(source)?;
    let escaped =
        converted.replace('\\', "\\\\").replace('"', "\\\"");
    let full = format!(
        "#set page(width: auto, height: auto, margin: 0pt, \
         fill: none)\n\
         #set text(size: {size_pt}pt, fill: white)\n\
         #import \"mitex-scope.typ\": mitex-scope\n\
         #math.equation(block: true, \
         eval(\"$ {escaped} $\", scope: mitex-scope))\n",
    );
    compile_source(&full)
}

pub fn latex_to_typst_math(
    source: &str,
) -> Result<String, CompileError> {
    mitex::convert_math(source, None).map_err(CompileError::LaTeX)
}

pub fn compile_source(
    source: &str,
) -> Result<MathFormula, CompileError> {
    let engine = TypstEngine::builder()
        .main_file(source.to_string())
        .with_static_source_file_resolver([(
            "mitex-scope.typ",
            MITEX_SCOPE,
        )])
        .fonts(embedded_fonts().iter().cloned())
        .build();

    let compiled = engine.compile::<PagedDocument>();
    let doc = compiled
        .output
        .map_err(|e| CompileError::Typst(format!("{e}")))?;
    let frame = &doc.pages.first().ok_or(CompileError::NoPage)?.frame;

    let mut paths = Vec::new();
    walk(frame, Affine::IDENTITY, &mut paths);

    let (w, h) = (frame.size().x.to_pt(), frame.size().y.to_pt());
    let recenter =
        Affine::new([1.0, 0.0, 0.0, -1.0, -w * 0.5, h * 0.5]);
    for sp in &mut paths {
        sp.path.apply_affine(recenter);
    }

    Ok(MathFormula {
        paths,
        size: (w, h),
    })
}

fn embedded_fonts() -> &'static [Font] {
    static FONTS: OnceLock<Vec<Font>> = OnceLock::new();
    FONTS.get_or_init(|| {
        typst_kit::fonts::Fonts::searcher()
            .include_system_fonts(false)
            .search()
            .fonts
            .iter()
            .filter_map(|slot| slot.get())
            .collect()
    })
}

fn walk(
    frame: &Frame,
    transform: Affine,
    out: &mut Vec<FormulaPath>,
) {
    for (pos, item) in frame.items() {
        let local = transform
            * Affine::translate((pos.x.to_pt(), pos.y.to_pt()));
        match item {
            FrameItem::Group(group) => {
                walk(
                    &group.frame,
                    local * to_affine(group.transform),
                    out,
                );
            }
            FrameItem::Text(text) => {
                let upem = text.font.units_per_em();
                let scale = text.size.to_pt() / upem;
                let face = text.font.ttf();
                let fill = paint_color(&text.fill);
                let family_hash = hash_str(&text.font.info().family);

                let mut x = 0.0f64;
                for glyph in &text.glyphs {
                    let dx = x + glyph.x_offset.at(text.size).to_pt();
                    let dy = -glyph.y_offset.at(text.size).to_pt();
                    let mut builder = OutlineToBez::default();
                    let outlined = face
                        .outline_glyph(
                            ttf_parser::GlyphId(glyph.id),
                            &mut builder,
                        )
                        .is_some();
                    if outlined {
                        let mut path = builder.path;
                        path.apply_affine(
                            local
                                * Affine::translate((dx, dy))
                                * Affine::scale_non_uniform(
                                    scale, -scale,
                                ),
                        );
                        out.push(FormulaPath {
                            path,
                            fill: Some(fill),
                            stroke: None,
                            key: family_hash ^ u64::from(glyph.id),
                        });
                    }
                    x += glyph.x_advance.at(text.size).to_pt();
                }
            }
            FrameItem::Shape(shape, _) => {
                let mut path = match &shape.geometry {
                    Geometry::Rect(size) => Rect::new(
                        0.0,
                        0.0,
                        size.x.to_pt(),
                        size.y.to_pt(),
                    )
                    .to_path(1e-3),
                    Geometry::Line(to) => {
                        let mut p = BezPath::new();
                        p.move_to((0.0, 0.0));
                        p.line_to((to.x.to_pt(), to.y.to_pt()));
                        p
                    }
                    Geometry::Curve(curve) => {
                        use typst::visualize::CurveItem;
                        let mut p = BezPath::new();
                        for item in &curve.0 {
                            match item {
                                CurveItem::Move(pt) => p.move_to((
                                    pt.x.to_pt(),
                                    pt.y.to_pt(),
                                )),
                                CurveItem::Line(pt) => p.line_to((
                                    pt.x.to_pt(),
                                    pt.y.to_pt(),
                                )),
                                CurveItem::Cubic(c1, c2, pt) => p
                                    .curve_to(
                                        (c1.x.to_pt(), c1.y.to_pt()),
                                        (c2.x.to_pt(), c2.y.to_pt()),
                                        (pt.x.to_pt(), pt.y.to_pt()),
                                    ),
                                CurveItem::Close => p.close_path(),
                            }
                        }
                        p
                    }
                };
                path.apply_affine(local);
                let kind = match &shape.geometry {
                    Geometry::Rect(_) => "rect",
                    Geometry::Line(_) => "line",
                    Geometry::Curve(_) => "curve",
                };
                out.push(FormulaPath {
                    path,
                    fill: shape.fill.as_ref().map(paint_color),
                    stroke: shape.stroke.as_ref().map(|s| {
                        (paint_color(&s.paint), s.thickness.to_pt())
                    }),
                    key: hash_str(kind),
                });
            }
            _ => {}
        }
    }
}

fn hash_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

fn to_affine(t: typst::layout::Transform) -> Affine {
    Affine::new([
        t.sx.get(),
        t.ky.get(),
        t.kx.get(),
        t.sy.get(),
        t.tx.to_pt(),
        t.ty.to_pt(),
    ])
}

fn paint_color(paint: &Paint) -> Color {
    match paint {
        Paint::Solid(color) => {
            let [r, g, b, a] =
                typst::visualize::Color::Rgb(color.to_rgb())
                    .to_vec4();
            Color::new([r, g, b, a])
        }
        _ => Color::WHITE,
    }
}

#[derive(Default)]
struct OutlineToBez {
    path: BezPath,
}

impl ttf_parser::OutlineBuilder for OutlineToBez {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path.move_to((f64::from(x), f64::from(y)));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to((f64::from(x), f64::from(y)));
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.path.quad_to(
            (f64::from(x1), f64::from(y1)),
            (f64::from(x), f64::from(y)),
        );
    }

    fn curve_to(
        &mut self,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        x: f32,
        y: f32,
    ) {
        self.path.curve_to(
            (f64::from(x1), f64::from(y1)),
            (f64::from(x2), f64::from(y2)),
            (f64::from(x), f64::from(y)),
        );
    }

    fn close(&mut self) {
        self.path.close_path();
    }
}
