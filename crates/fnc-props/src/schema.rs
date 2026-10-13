//! The property table (SPEC ch.2 §5.5–5.9, appendix B).
//!
//! Every searchable / displayable property has a [`PropertyId`] (an index into
//! [`PROPERTIES`]) with its canonical search name, aliases (matched after removing `-`/`_`
//! and lowercasing, so `camera-model:` ≡ `cameramodel:`), column title, value type,
//! extractor hint and the Windows Property System canonical name (`System.*`) that the
//! injected [`crate::PropertyProvider`] fallback uses.
//!
//! SPEC-GAP: appendix B asks for all ~300 Everything 1.5 property names. This table
//! registers the properties of SPEC ch.2 §5.5–5.9 (plus a few derived ones); others can
//! be appended without format changes (ids are positional, persisted by canonical name).

use std::collections::HashMap;
use std::fmt;
use std::sync::LazyLock;

/// Identifier of a property (index into [`PROPERTIES`]).
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PropertyId(pub u16);

impl fmt::Debug for PropertyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// Value type of a property.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PropType {
    /// Signed integer.
    Int,
    /// Floating point.
    Float,
    /// Text.
    Text,
    /// Date (`FILETIME`, UTC).
    Date,
    /// Duration in milliseconds.
    Duration,
    /// Several texts (keywords, authors).
    MultiText,
    /// Yes/no, stored as `Int(0|1)`.
    Bool,
}

/// Which extractor produces a property (scheduling hint; the Windows Property System
/// fallback can produce any property that has a `system` name).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Extractor {
    /// Image header (dimensions, bit depth).
    Image,
    /// EXIF block of photos.
    Exif,
    /// Audio tags and stream properties.
    Audio,
    /// Video containers (MP4/MOV, Matroska/WebM).
    Video,
    /// Office Open XML and PDF metadata.
    Document,
    /// PE headers and version resources.
    Executable,
    /// Computed from other properties (no extraction).
    Derived,
    /// Only available through the Windows Property System fallback.
    System,
}

/// Display/parse unit of numeric properties.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Unit {
    /// Plain number.
    None,
    /// Pixels.
    Pixels,
    /// Dots per inch.
    Dpi,
    /// Bits (bit depth, sample size).
    Bits,
    /// Kilobits per second.
    Kbps,
    /// Hertz.
    Hz,
    /// Millimetres (focal length).
    Mm,
    /// Seconds (exposure time, shown as a fraction).
    Seconds,
    /// F-number (`f/2.8`).
    FNumber,
    /// Degrees (GPS, signed decimal).
    Degrees,
    /// Metres (altitude).
    Meters,
    /// Frames per second.
    Fps,
    /// Star rating 0–5.
    Stars,
}

/// One row of the property table.
#[derive(Debug)]
pub struct PropertyDef {
    /// Id (position in [`PROPERTIES`]).
    pub id: PropertyId,
    /// Canonical search name (`camera-model`).
    pub canonical: &'static str,
    /// Further search names (canonical excluded), as written in Everything's docs.
    pub aliases: &'static [&'static str],
    /// Column title (`Camera model`).
    pub title: &'static str,
    /// Value type.
    pub ty: PropType,
    /// Extractor hint.
    pub extractor: Extractor,
    /// Unit for display and value parsing.
    pub unit: Unit,
    /// Windows Property System canonical name (`System.Photo.CameraModel`), empty if none.
    pub system: &'static str,
}

macro_rules! properties {
    ($( $k:ident : $canon:literal [$($alias:literal),*] $title:literal $ty:ident $ex:ident $unit:ident $sys:literal ;)*) => {
        #[allow(non_camel_case_types, clippy::upper_case_acronyms, dead_code)]
        #[repr(u16)]
        enum Ix { $($k),* }

        impl PropertyId {
            $(
                #[doc = concat!("`", $canon, ":` — ", $title)]
                pub const $k: PropertyId = PropertyId(Ix::$k as u16);
            )*
        }

        /// Every property, indexed by [`PropertyId`].
        pub static PROPERTIES: &[PropertyDef] = &[
            $(PropertyDef {
                id: PropertyId::$k,
                canonical: $canon,
                aliases: &[$($alias),*],
                title: $title,
                ty: PropType::$ty,
                extractor: Extractor::$ex,
                unit: Unit::$unit,
                system: $sys,
            }),*
        ];
    };
}

properties! {
    // ---- image (SPEC ch.2 §5.5) ----
    WIDTH: "width" [] "Width" Int Image Pixels "System.Image.HorizontalSize";
    HEIGHT: "height" [] "Height" Int Image Pixels "System.Image.VerticalSize";
    DIMENSIONS: "dimensions" [] "Dimensions" Text Derived None "System.Image.Dimensions";
    ASPECT_RATIO: "aspect-ratio" [] "Aspect ratio" Float Derived None "";
    BIT_DEPTH: "bit-depth" [] "Bit depth" Int Image Bits "System.Image.BitDepth";
    HORIZONTAL_RESOLUTION: "horizontal-resolution" [] "Horizontal resolution" Float Exif Dpi "System.Image.HorizontalResolution";
    VERTICAL_RESOLUTION: "vertical-resolution" [] "Vertical resolution" Float Exif Dpi "System.Image.VerticalResolution";
    ORIENTATION: "orientation" [] "Orientation" Int Exif None "System.Photo.Orientation";
    DATE_TAKEN: "date-taken" [] "Date taken" Date Exif None "System.Photo.DateTaken";
    CAMERA_MAKER: "camera-maker" ["camera-make"] "Camera maker" Text Exif None "System.Photo.CameraManufacturer";
    CAMERA_MODEL: "camera-model" [] "Camera model" Text Exif None "System.Photo.CameraModel";
    EXPOSURE_TIME: "exposure-time" [] "Exposure time" Float Exif Seconds "System.Photo.ExposureTime";
    F_STOP: "f-stop" ["f-number"] "F-stop" Float Exif FNumber "System.Photo.FNumber";
    ISO_SPEED: "iso-speed" [] "ISO speed" Int Exif None "System.Photo.ISOSpeed";
    FOCAL_LENGTH: "focal-length" [] "Focal length" Float Exif Mm "System.Photo.FocalLength";
    FLASH: "flash" [] "Flash" Bool Exif None "System.Photo.Flash";
    LATITUDE: "latitude" [] "Latitude" Float Exif Degrees "System.GPS.Latitude";
    LONGITUDE: "longitude" [] "Longitude" Float Exif Degrees "System.GPS.Longitude";
    ALTITUDE: "altitude" [] "Altitude" Float Exif Meters "System.GPS.Altitude";
    PEOPLE: "people" [] "People" MultiText System None "System.Photo.PeopleNames";
    TAGS: "tags" ["keywords"] "Tags" MultiText Document None "System.Keywords";
    RATING: "rating" [] "Rating" Int Audio Stars "System.Rating";
    TITLE: "title" [] "Title" Text Document None "System.Title";
    SUBJECT: "subject" [] "Subject" Text Document None "System.Subject";
    COMMENT: "comment" ["comments"] "Comments" Text Document None "System.Comment";
    COPYRIGHT: "copyright" ["legal-copyright"] "Copyright" Text Exif None "System.Copyright";
    // ---- audio (§5.6) ----
    LENGTH: "length" ["duration"] "Length" Duration Audio None "System.Media.Duration";
    ARTIST: "artist" [] "Artist" Text Audio None "System.Music.Artist";
    ALBUM: "album" [] "Album" Text Audio None "System.Music.AlbumTitle";
    ALBUM_ARTIST: "album-artist" [] "Album artist" Text Audio None "System.Music.AlbumArtist";
    TRACK: "track" ["track-number"] "#" Int Audio None "System.Music.TrackNumber";
    DISC: "disc" ["disc-number"] "Disc" Int Audio None "System.Music.DiscNumber";
    GENRE: "genre" [] "Genre" Text Audio None "System.Music.Genre";
    YEAR: "year" [] "Year" Int Audio None "System.Media.Year";
    COMPOSER: "composer" [] "Composer" Text Audio None "System.Music.Composer";
    CONDUCTOR: "conductor" [] "Conductor" Text Audio None "System.Music.Conductor";
    PUBLISHER: "publisher" [] "Publisher" Text Audio None "System.Media.Publisher";
    BIT_RATE: "bit-rate" ["encoding-bitrate", "audio-bitrate"] "Bit rate" Int Audio Kbps "System.Audio.EncodingBitrate";
    SAMPLE_RATE: "sample-rate" [] "Sample rate" Int Audio Hz "System.Audio.SampleRate";
    CHANNELS: "channels" [] "Channels" Int Audio None "System.Audio.ChannelCount";
    BITS_PER_SAMPLE: "bits-per-sample" ["sample-size"] "Bits per sample" Int Audio Bits "System.Audio.SampleSize";
    IS_VARIABLE_BIT_RATE: "is-variable-bit-rate" ["vbr"] "Variable bit rate" Bool Audio None "System.Audio.IsVariableBitRate";
    LYRICS: "lyrics" [] "Lyrics" Text Audio None "System.Music.Lyrics";
    MOOD: "mood" [] "Mood" Text Audio None "System.Music.Mood";
    BPM: "bpm" ["beats-per-minute"] "Beats per minute" Int Audio None "System.Music.BeatsPerMinute";
    INITIAL_KEY: "initial-key" [] "Initial key" Text Audio None "System.Music.InitialKey";
    ENCODED_BY: "encoded-by" [] "Encoded by" Text Audio None "System.Media.EncodedBy";
    // ---- video (§5.7) ----
    FRAME_WIDTH: "frame-width" [] "Frame width" Int Video Pixels "System.Video.FrameWidth";
    FRAME_HEIGHT: "frame-height" [] "Frame height" Int Video Pixels "System.Video.FrameHeight";
    FRAME_RATE: "frame-rate" ["fps"] "Frame rate" Float Video Fps "System.Video.FrameRate";
    VIDEO_BITRATE: "video-bitrate" ["data-rate"] "Data rate" Int Video Kbps "System.Video.EncodingBitrate";
    TOTAL_BITRATE: "total-bitrate" [] "Total bitrate" Int Video Kbps "System.Video.TotalBitrate";
    COMPRESSION: "compression" [] "Video compression" Text Video None "System.Video.Compression";
    VIDEO_CODEC: "video-codec" [] "Video codec" Text Video None "";
    AUDIO_CODEC: "audio-codec" [] "Audio codec" Text Video None "";
    DATE_ENCODED: "date-encoded" [] "Date encoded" Date Video None "System.Media.DateEncoded";
    DIRECTOR: "director" [] "Directors" Text Video None "System.Video.Director";
    PRODUCER: "producer" [] "Producers" Text Video None "System.Media.Producer";
    WRITER: "writer" [] "Writers" Text Video None "System.Media.Writer";
    STREAM_COUNT: "stream-count" [] "Streams" Int Video None "";
    FOUR_CC: "four-cc" [] "FourCC" Text Video None "System.Video.FourCC";
    // ---- document (§5.8) ----
    AUTHOR: "author" ["authors"] "Authors" MultiText Document None "System.Author";
    LAST_AUTHOR: "last-author" ["last-saved-by"] "Last saved by" Text Document None "System.Document.LastAuthor";
    CATEGORY: "category" [] "Category" Text Document None "System.Category";
    COMPANY: "company" ["company-name"] "Company" Text Document None "System.Company";
    MANAGER: "manager" [] "Manager" Text Document None "System.Document.Manager";
    PAGE_COUNT: "page-count" ["pages"] "Pages" Int Document None "System.Document.PageCount";
    SLIDE_COUNT: "slide-count" ["slides"] "Slides" Int Document None "System.Presentation.SlideCount";
    WORD_COUNT: "word-count" [] "Word count" Int Document None "System.Document.WordCount";
    CHAR_COUNT: "char-count" ["character-count"] "Character count" Int Document None "System.Document.CharacterCount";
    LINE_COUNT: "line-count" [] "Line count" Int Document None "System.Document.LineCount";
    PARAGRAPH_COUNT: "paragraph-count" [] "Paragraph count" Int Document None "System.Document.ParagraphCount";
    REVISION_NUMBER: "revision-number" ["revision"] "Revision number" Text Document None "System.Document.RevisionNumber";
    TEMPLATE: "template" [] "Template" Text Document None "System.Document.Template";
    DATE_PRINTED: "date-printed" [] "Date last printed" Date Document None "System.Document.DatePrinted";
    DATE_LAST_SAVED: "date-last-saved" ["date-saved"] "Date last saved" Date Document None "System.Document.DateSaved";
    CONTENT_CREATED: "content-created" ["date-content-created"] "Content created" Date Document None "System.Document.DateCreated";
    TOTAL_EDITING_TIME: "total-editing-time" [] "Total editing time" Duration Document None "System.Document.TotalEditingTime";
    SECURITY: "security" [] "Security" Int Document None "System.Document.Security";
    // ---- executable (§5.9) ----
    FILE_VERSION: "file-version" [] "File version" Text Executable None "System.FileVersion";
    PRODUCT_VERSION: "product-version" [] "Product version" Text Executable None "System.Software.ProductVersion";
    PRODUCT_NAME: "product-name" [] "Product name" Text Executable None "System.Software.ProductName";
    FILE_DESCRIPTION: "file-description" [] "File description" Text Executable None "System.FileDescription";
    ORIGINAL_FILENAME: "original-filename" [] "Original filename" Text Executable None "System.OriginalFileName";
    INTERNAL_NAME: "internal-name" [] "Internal name" Text Executable None "";
    LEGAL_TRADEMARKS: "legal-trademarks" [] "Legal trademarks" Text Executable None "System.Trademarks";
    PRIVATE_BUILD: "private-build" [] "Private build" Text Executable None "";
    SPECIAL_BUILD: "special-build" [] "Special build" Text Executable None "";
    LANGUAGE: "language" [] "Language" Text Executable None "System.Language";
    MACHINE: "machine" [] "Machine" Int Executable None "";
    SUBSYSTEM: "subsystem" [] "Subsystem" Int Executable None "";
    IS_DLL: "is-dll" [] "DLL" Bool Executable None "";
    IS_SIGNED: "is-signed" [] "Signed" Bool Executable None "";
    LINKER_VERSION: "linker-version" [] "Linker version" Text Executable None "";
    OS_VERSION: "os-version" [] "OS version" Text Executable None "";
    IMAGE_VERSION: "image-version" [] "Image version" Text Executable None "";
}

/// Normalises a search name: ASCII lowercase, `-`/`_` removed (like function names).
pub fn normalize(name: &str) -> String {
    name.chars().filter(|c| *c != '-' && *c != '_').map(|c| c.to_ascii_lowercase()).collect()
}

static BY_NAME: LazyLock<HashMap<String, PropertyId>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    for d in PROPERTIES {
        for n in std::iter::once(&d.canonical).chain(d.aliases) {
            m.entry(normalize(n)).or_insert(d.id);
        }
    }
    m
});

impl PropertyId {
    /// Number of registered properties.
    pub fn count() -> usize {
        PROPERTIES.len()
    }

    /// All properties.
    pub fn all() -> impl Iterator<Item = PropertyId> {
        PROPERTIES.iter().map(|d| d.id)
    }

    /// Table row (`None` for an id outside the table).
    pub fn def(self) -> Option<&'static PropertyDef> {
        PROPERTIES.get(self.0 as usize)
    }

    /// Canonical search name (`"?"` for an id outside the table).
    pub fn name(self) -> &'static str {
        self.def().map_or("?", |d| d.canonical)
    }

    /// Column title.
    pub fn title(self) -> &'static str {
        self.def().map_or("?", |d| d.title)
    }

    /// Value type.
    pub fn ty(self) -> PropType {
        self.def().map_or(PropType::Text, |d| d.ty)
    }

    /// Unit.
    pub fn unit(self) -> Unit {
        self.def().map_or(Unit::None, |d| d.unit)
    }

    /// Extractor hint.
    pub fn extractor(self) -> Extractor {
        self.def().map_or(Extractor::System, |d| d.extractor)
    }

    /// `true` for properties computed from others (never stored).
    pub fn is_derived(self) -> bool {
        self.extractor() == Extractor::Derived
    }

    /// Looks up a search name (any alias, dashes/underscores/case ignored).
    pub fn lookup(name: &str) -> Option<PropertyId> {
        let norm = if name.bytes().any(|b| b == b'-' || b == b'_' || b.is_ascii_uppercase()) {
            normalize(name)
        } else {
            name.to_string()
        };
        BY_NAME.get(&norm).copied()
    }

    /// All `(normalised alias, id)` pairs (for function tables and completion).
    pub fn search_names() -> impl Iterator<Item = (String, PropertyId)> {
        PROPERTIES.iter().flat_map(|d| std::iter::once(&d.canonical).chain(d.aliases).map(|n| (normalize(n), d.id)))
    }

    /// Named values of enumerated integer properties (`machine:x64`, `orientation:normal`).
    pub fn value_names(self) -> &'static [(&'static str, i64)] {
        match self {
            PropertyId::ORIENTATION => &[
                ("normal", 1),
                ("flip-horizontal", 2),
                ("rotate-180", 3),
                ("flip-vertical", 4),
                ("transpose", 5),
                ("rotate-90", 6),
                ("transverse", 7),
                ("rotate-270", 8),
            ],
            PropertyId::MACHINE => &[
                ("x86", 0x14c),
                ("i386", 0x14c),
                ("x64", 0x8664),
                ("amd64", 0x8664),
                ("arm64", 0xaa64),
                ("arm", 0x1c4),
                ("armnt", 0x1c4),
                ("ia64", 0x200),
                ("arm64ec", 0xa641),
            ],
            PropertyId::SUBSYSTEM => &[
                ("native", 1),
                ("windows", 2),
                ("gui", 2),
                ("console", 3),
                ("cui", 3),
                ("posix", 7),
                ("windows-ce", 9),
                ("efi-application", 10),
                ("efi", 10),
                ("efi-boot-driver", 11),
                ("efi-runtime-driver", 12),
                ("boot-application", 16),
            ],
            PropertyId::SECURITY => {
                &[("none", 0), ("password", 1), ("read-only-recommended", 2), ("read-only", 4), ("locked", 8)]
            }
            _ => &[],
        }
    }

    /// Value of a named constant (`"x64"` → `0x8664`), dashes and case ignored.
    pub fn named_value(self, text: &str) -> Option<i64> {
        let n = normalize(text.trim());
        self.value_names().iter().find(|(k, _)| normalize(k) == n).map(|&(_, v)| v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_positional_and_names_unique() {
        let mut seen = std::collections::HashSet::new();
        for (i, d) in PROPERTIES.iter().enumerate() {
            assert_eq!(d.id.0 as usize, i);
            for n in std::iter::once(&d.canonical).chain(d.aliases) {
                assert!(seen.insert(normalize(n)), "duplicate property name {n}");
            }
            assert!(!d.title.is_empty());
        }
        assert!(PropertyId::count() >= 90);
    }

    #[test]
    fn lookup_ignores_dashes_and_case() {
        assert_eq!(PropertyId::lookup("camera-model"), Some(PropertyId::CAMERA_MODEL));
        assert_eq!(PropertyId::lookup("CameraModel"), Some(PropertyId::CAMERA_MODEL));
        assert_eq!(PropertyId::lookup("keywords"), Some(PropertyId::TAGS));
        assert_eq!(PropertyId::lookup("duration"), Some(PropertyId::LENGTH));
        assert_eq!(PropertyId::lookup("fps"), Some(PropertyId::FRAME_RATE));
        assert_eq!(PropertyId::lookup("nope"), None);
        assert_eq!(PropertyId::WIDTH.name(), "width");
        assert_eq!(PropertyId::WIDTH.ty(), PropType::Int);
        assert!(PropertyId::DIMENSIONS.is_derived());
        assert_eq!(format!("{:?}", PropertyId::F_STOP), "f-stop");
        assert_eq!(PropertyId(9999).name(), "?");
    }

    #[test]
    fn spec_function_names_are_registered() {
        // SPEC ch.2 §5.5–5.9 function names.
        let names = "width height dimensions aspect-ratio bit-depth horizontal-resolution vertical-resolution \
            orientation date-taken camera-maker camera-model exposure-time f-stop iso-speed focal-length flash \
            latitude longitude altitude people tags keywords rating title subject comment copyright \
            length duration artist album album-artist track track-number disc genre year composer conductor \
            publisher bit-rate sample-rate channels bits-per-sample encoding-bitrate is-variable-bit-rate lyrics \
            mood bpm initial-key encoded-by frame-width frame-height frame-rate fps video-bitrate total-bitrate \
            compression video-codec audio-codec date-encoded director producer writer stream-count four-cc \
            data-rate author authors last-author category comments company manager page-count slide-count \
            word-count char-count line-count paragraph-count revision-number template date-printed \
            date-last-saved content-created total-editing-time security file-version product-version \
            product-name company-name file-description original-filename internal-name legal-copyright \
            legal-trademarks private-build special-build language machine subsystem is-dll is-signed \
            linker-version os-version image-version";
        for n in names.split_whitespace() {
            assert!(PropertyId::lookup(n).is_some(), "{n} not registered");
        }
    }

    #[test]
    fn named_values() {
        assert_eq!(PropertyId::MACHINE.named_value("x64"), Some(0x8664));
        assert_eq!(PropertyId::MACHINE.named_value("ARM64"), Some(0xaa64));
        assert_eq!(PropertyId::ORIENTATION.named_value("rotate90"), Some(6));
        assert_eq!(PropertyId::WIDTH.named_value("x"), None);
    }
}
