//! The property table (SPEC ch.2 §5.5–5.9, appendix B).
//!
//! Every searchable / displayable property has a [`PropertyId`] (an index into
//! [`PROPERTIES`]) with its canonical search name, aliases (matched after removing `-`/`_`
//! and lowercasing, so `camera-model:` ≡ `cameramodel:`), column title, value type,
//! extractor hint and the Windows Property System canonical name (`System.*`) that the
//! injected [`crate::PropertyProvider`] fallback uses.
//!
//! The table holds about 300 properties (SPEC ch.2 appendix B): the functions of SPEC ch.2
//! §5.5–5.9, two derived ones (`dimensions`, `aspect-ratio`) and the commonly used Windows
//! shell properties (core, contacts, e-mail, calendar, media, music, photo/EXIF, GPS,
//! video, audio, document, shortcut). Canonical names are Explorer's English column titles
//! in Everything's dashed lowercase style (`lens-maker`); the aliases add the `System.*`
//! short names (`lens-manufacturer`) and prefixed forms (`contact-first-name`,
//! `message-date-received`). Properties whose extractor is [`Extractor::System`] are read
//! only through the Windows Property System; every such entry has a `System.*` name.
//!
//! New entries are appended: ids are positional in memory but persisted by canonical
//! name, so the table can grow (or be reordered) without a format change. Canonical
//! names of existing entries must not change (`props.fnx` columns are matched by them).
//!
//! SPEC-GAP: Everything 1.5's own list of property names is not published; names that
//! collide with built-in search functions (`kind:`, `link-target:`, `offline:`) are not
//! registered as properties (`kind-text:` reads `System.KindText`).

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
    ISO_SPEED: "iso-speed" ["iso"] "ISO speed" Int Exif None "System.Photo.ISOSpeed";
    FOCAL_LENGTH: "focal-length" [] "Focal length" Float Exif Mm "System.Photo.FocalLength";
    FLASH: "flash" [] "Flash" Bool Exif None "System.Photo.Flash";
    LATITUDE: "latitude" ["gps-latitude"] "Latitude" Float Exif Degrees "System.GPS.Latitude";
    LONGITUDE: "longitude" ["gps-longitude"] "Longitude" Float Exif Degrees "System.GPS.Longitude";
    ALTITUDE: "altitude" ["gps-altitude"] "Altitude" Float Exif Meters "System.GPS.Altitude";
    PEOPLE: "people" ["people-names", "people-tags"] "People" MultiText System None "System.Photo.PeopleNames";
    TAGS: "tags" ["keywords"] "Tags" MultiText Document None "System.Keywords";
    RATING: "rating" ["star-rating"] "Rating" Int Audio Stars "System.Rating";
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
    SAMPLE_RATE: "sample-rate" ["audio-sample-rate"] "Sample rate" Int Audio Hz "System.Audio.SampleRate";
    CHANNELS: "channels" ["audio-channels", "channel-count"] "Channels" Int Audio None "System.Audio.ChannelCount";
    BITS_PER_SAMPLE: "bits-per-sample" ["sample-size", "audio-sample-size"] "Bits per sample" Int Audio Bits "System.Audio.SampleSize";
    IS_VARIABLE_BIT_RATE: "is-variable-bit-rate" ["vbr"] "Variable bit rate" Bool Audio None "System.Audio.IsVariableBitRate";
    LYRICS: "lyrics" [] "Lyrics" Text Audio None "System.Music.Lyrics";
    MOOD: "mood" [] "Mood" Text Audio None "System.Music.Mood";
    BPM: "bpm" ["beats-per-minute"] "Beats per minute" Int Audio None "System.Music.BeatsPerMinute";
    INITIAL_KEY: "initial-key" [] "Initial key" Text Audio None "System.Music.InitialKey";
    ENCODED_BY: "encoded-by" [] "Encoded by" Text Audio None "System.Media.EncodedBy";
    // ---- video (§5.7) ----
    FRAME_WIDTH: "frame-width" ["video-width"] "Frame width" Int Video Pixels "System.Video.FrameWidth";
    FRAME_HEIGHT: "frame-height" ["video-height"] "Frame height" Int Video Pixels "System.Video.FrameHeight";
    FRAME_RATE: "frame-rate" ["fps"] "Frame rate" Float Video Fps "System.Video.FrameRate";
    VIDEO_BITRATE: "video-bitrate" ["data-rate"] "Data rate" Int Video Kbps "System.Video.EncodingBitrate";
    TOTAL_BITRATE: "total-bitrate" [] "Total bitrate" Int Video Kbps "System.Video.TotalBitrate";
    COMPRESSION: "compression" ["video-compression"] "Video compression" Text Video None "System.Video.Compression";
    VIDEO_CODEC: "video-codec" [] "Video codec" Text Video None "";
    AUDIO_CODEC: "audio-codec" [] "Audio codec" Text Video None "";
    DATE_ENCODED: "date-encoded" [] "Date encoded" Date Video None "System.Media.DateEncoded";
    DIRECTOR: "director" ["directors"] "Directors" Text Video None "System.Video.Director";
    PRODUCER: "producer" ["producers"] "Producers" Text Video None "System.Media.Producer";
    WRITER: "writer" ["writers"] "Writers" Text Video None "System.Media.Writer";
    STREAM_COUNT: "stream-count" [] "Streams" Int Video None "";
    FOUR_CC: "four-cc" ["video-fourcc"] "FourCC" Text Video None "System.Video.FourCC";
    // ---- document (§5.8) ----
    AUTHOR: "author" ["authors"] "Authors" MultiText Document None "System.Author";
    LAST_AUTHOR: "last-author" ["last-saved-by"] "Last saved by" Text Document None "System.Document.LastAuthor";
    CATEGORY: "category" [] "Category" Text Document None "System.Category";
    COMPANY: "company" ["company-name"] "Company" Text Document None "System.Company";
    MANAGER: "manager" [] "Manager" Text Document None "System.Document.Manager";
    PAGE_COUNT: "page-count" ["pages"] "Pages" Int Document None "System.Document.PageCount";
    SLIDE_COUNT: "slide-count" ["slides"] "Slides" Int Document None "System.Document.SlideCount";
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
    // ---- appendix B: further Windows shell properties (mostly via the Windows provider) ----
    // core
    PROGRAM_NAME: "program-name" ["application-name", "application"] "Program name" Text Document None "System.ApplicationName";
    COMPUTER: "computer" ["computer-name"] "Computer" Text System None "System.ComputerName";
    CONTENT_STATUS: "content-status" [] "Content status" Text Document None "System.ContentStatus";
    CONTENT_TYPE: "content-type" [] "Content type" Text System None "System.ContentType";
    DATE_ACQUIRED: "date-acquired" [] "Date acquired" Date System None "System.DateAcquired";
    DATE_ARCHIVED: "date-archived" [] "Date archived" Date System None "System.DateArchived";
    DATE_COMPLETED: "date-completed" [] "Date completed" Date System None "System.DateCompleted";
    DATE_IMPORTED: "date-imported" [] "Date imported" Date System None "System.DateImported";
    DUE_DATE: "due-date" [] "Due date" Date System None "System.DueDate";
    START_DATE: "start-date" [] "Start date" Date System None "System.StartDate";
    END_DATE: "end-date" [] "End date" Date System None "System.EndDate";
    ITEM_DATE: "item-date" [] "Date" Date System None "System.ItemDate";
    FLAG_STATUS: "flag-status" [] "Flag status" Int System None "System.FlagStatus";
    FLAG_COLOR: "flag-color" [] "Flag color" Int System None "System.FlagColor";
    IMPORTANCE: "importance" [] "Importance" Int System None "System.Importance";
    PRIORITY: "priority" [] "Priority" Int System None "System.Priority";
    SENSITIVITY: "sensitivity" [] "Sensitivity" Int System None "System.Sensitivity";
    IS_ATTACHMENT: "is-attachment" [] "Is attachment" Bool System None "System.IsAttachment";
    IS_FLAGGED: "is-flagged" [] "Flagged" Bool System None "System.IsFlagged";
    IS_FLAGGED_COMPLETE: "is-flagged-complete" [] "Flag completed" Bool System None "System.IsFlaggedComplete";
    IS_INCOMPLETE: "is-incomplete" [] "Incomplete" Bool System None "System.IsIncomplete";
    IS_READ: "is-read" [] "Read" Bool System None "System.IsRead";
    IS_SHARED: "is-shared" [] "Shared" Bool System None "System.IsShared";
    SHARED_WITH: "shared-with" [] "Shared with" MultiText System None "System.SharedWith";
    SHARING_STATUS: "sharing-status" ["sharing"] "Sharing status" Int System None "System.SharingStatus";
    ITEM_TYPE: "item-type" ["item-type-text"] "Item type" Text System None "System.ItemTypeText";
    KIND_TEXT: "kind-text" ["item-kind"] "Kind" Text System None "System.KindText";
    PERCEIVED_TYPE: "perceived-type" [] "Perceived type" Int System None "System.PerceivedType";
    MIME_TYPE: "mime-type" ["mime"] "MIME type" Text System None "System.MIMEType";
    STATUS: "status" [] "Status" Text System None "System.Status";
    PROJECT: "project" [] "Project" Text System None "System.Project";
    MILEAGE: "mileage" ["mileage-information"] "Mileage" Text System None "System.MileageInformation";
    PARTICIPANTS: "participants" ["item-participants"] "Participants" MultiText System None "System.ItemParticipants";
    OWNER: "owner" ["file-owner"] "Owner" Text System None "System.FileOwner";
    OFFLINE_AVAILABILITY: "offline-availability" ["availability"] "Availability" Int System None "System.OfflineAvailability";
    OFFLINE_STATUS: "offline-status" [] "Offline status" Int System None "System.OfflineStatus";
    PARENTAL_RATING: "parental-rating" [] "Parental rating" Text System None "System.ParentalRating";
    PARENTAL_RATING_REASON: "parental-rating-reason" [] "Parental rating reason" Text System None "System.ParentalRatingReason";
    SOFTWARE_USED: "software-used" ["software"] "Software used" Text Exif None "System.SoftwareUsed";
    DATE_LAST_USED: "date-last-used" [] "Date last used" Date System None "System.Software.DateLastUsed";
    IS_PROTECTED: "is-protected" ["drm-is-protected"] "Protected" Bool System None "System.DRM.IsProtected";
    PLAY_COUNT: "play-count" ["drm-play-count"] "Play count" Int System None "System.DRM.PlayCount";
    // contacts
    FULL_NAME: "full-name" ["contact-full-name"] "Full name" Text System None "System.Contact.FullName";
    FIRST_NAME: "first-name" ["contact-first-name", "given-name"] "First name" Text System None "System.Contact.FirstName";
    MIDDLE_NAME: "middle-name" ["contact-middle-name"] "Middle name" Text System None "System.Contact.MiddleName";
    LAST_NAME: "last-name" ["contact-last-name", "surname"] "Last name" Text System None "System.Contact.LastName";
    NICKNAME: "nickname" ["contact-nickname"] "Nickname" Text System None "System.Contact.NickName";
    JOB_TITLE: "job-title" ["contact-job-title"] "Job title" Text System None "System.Contact.JobTitle";
    DEPARTMENT: "department" ["contact-department"] "Department" Text System None "System.Contact.Department";
    OFFICE_LOCATION: "office-location" ["contact-office-location"] "Office location" Text System None "System.Contact.OfficeLocation";
    PROFESSION: "profession" ["contact-profession"] "Profession" Text System None "System.Contact.Profession";
    GENDER: "gender" ["contact-gender"] "Gender" Text System None "System.Contact.Gender";
    BIRTHDAY: "birthday" ["contact-birthday"] "Birthday" Date System None "System.Contact.Birthday";
    EMAIL: "email" ["contact-email", "primary-email"] "E-mail" Text System None "System.Contact.PrimaryEmailAddress";
    EMAIL_ADDRESSES: "email-addresses" ["contact-email-addresses"] "E-mail addresses" MultiText System None "System.Contact.EmailAddresses";
    IM_ADDRESS: "im-address" ["contact-im-address"] "IM address" MultiText System None "System.Contact.IMAddress";
    WEB_PAGE: "web-page" ["contact-web-page"] "Web page" Text System None "System.Contact.WebPage";
    PRIMARY_PHONE: "primary-phone" ["contact-primary-telephone", "phone"] "Primary phone" Text System None "System.Contact.PrimaryTelephone";
    BUSINESS_PHONE: "business-phone" ["contact-business-telephone"] "Business phone" Text System None "System.Contact.BusinessTelephone";
    HOME_PHONE: "home-phone" ["contact-home-telephone"] "Home phone" Text System None "System.Contact.HomeTelephone";
    MOBILE_PHONE: "mobile-phone" ["contact-mobile-telephone", "cell-phone"] "Mobile phone" Text System None "System.Contact.MobileTelephone";
    BUSINESS_ADDRESS: "business-address" ["contact-business-address"] "Business address" Text System None "System.Contact.BusinessAddress";
    BUSINESS_STREET: "business-street" ["contact-business-street"] "Business street" Text System None "System.Contact.BusinessAddressStreet";
    BUSINESS_CITY: "business-city" ["contact-business-city"] "Business city" Text System None "System.Contact.BusinessAddressCity";
    BUSINESS_STATE: "business-state" ["contact-business-state"] "Business state or province" Text System None "System.Contact.BusinessAddressState";
    BUSINESS_POSTAL_CODE: "business-postal-code" ["contact-business-postal-code"] "Business postal code" Text System None "System.Contact.BusinessAddressPostalCode";
    BUSINESS_COUNTRY: "business-country" ["contact-business-country"] "Business country/region" Text System None "System.Contact.BusinessAddressCountry";
    HOME_ADDRESS: "home-address" ["contact-home-address"] "Home address" Text System None "System.Contact.HomeAddress";
    HOME_STREET: "home-street" ["contact-home-street"] "Home street" Text System None "System.Contact.HomeAddressStreet";
    HOME_CITY: "home-city" ["contact-home-city"] "Home city" Text System None "System.Contact.HomeAddressCity";
    HOME_STATE: "home-state" ["contact-home-state"] "Home state or province" Text System None "System.Contact.HomeAddressState";
    HOME_POSTAL_CODE: "home-postal-code" ["contact-home-postal-code"] "Home postal code" Text System None "System.Contact.HomeAddressPostalCode";
    HOME_COUNTRY: "home-country" ["contact-home-country"] "Home country/region" Text System None "System.Contact.HomeAddressCountry";
    // messages (e-mail)
    FROM: "from" ["from-name", "message-from"] "From" Text System None "System.Message.FromName";
    FROM_ADDRESS: "from-address" ["message-from-address"] "From address" MultiText System None "System.Message.FromAddress";
    TO: "to" ["to-name", "message-to"] "To" MultiText System None "System.Message.ToName";
    TO_ADDRESS: "to-address" ["message-to-address"] "To address" MultiText System None "System.Message.ToAddress";
    CC: "cc" ["cc-name", "message-cc"] "CC" MultiText System None "System.Message.CcName";
    CC_ADDRESS: "cc-address" ["message-cc-address"] "CC address" MultiText System None "System.Message.CcAddress";
    BCC: "bcc" ["bcc-name", "message-bcc"] "BCC" MultiText System None "System.Message.BccName";
    BCC_ADDRESS: "bcc-address" ["message-bcc-address"] "BCC address" MultiText System None "System.Message.BccAddress";
    SENDER: "sender" ["sender-name", "message-sender"] "Sender name" Text System None "System.Message.SenderName";
    SENDER_ADDRESS: "sender-address" ["message-sender-address"] "Sender address" Text System None "System.Message.SenderAddress";
    DATE_RECEIVED: "date-received" ["message-date-received"] "Date received" Date System None "System.Message.DateReceived";
    DATE_SENT: "date-sent" ["message-date-sent"] "Date sent" Date System None "System.Message.DateSent";
    HAS_ATTACHMENTS: "has-attachments" ["message-has-attachments"] "Attachments" Bool System None "System.Message.HasAttachments";
    ATTACHMENT_NAMES: "attachment-names" ["message-attachment-names"] "Attachment names" MultiText System None "System.Message.AttachmentNames";
    MESSAGE_CLASS: "message-class" [] "Message class" Text System None "System.Message.MessageClass";
    IS_FWD_OR_REPLY: "is-fwd-or-reply" ["message-is-fwd-or-reply"] "Forwarded or replied" Bool System None "System.Message.IsFwdOrReply";
    TO_DO_TITLE: "to-do-title" ["message-to-do-title"] "To-do title" Text System None "System.Message.ToDoTitle";
    // calendar and tasks
    LOCATION: "location" ["meeting-location", "calendar-location"] "Location" Text System None "System.Calendar.Location";
    ORGANIZER: "organizer" ["organizer-name", "calendar-organizer"] "Organizer" Text System None "System.Calendar.OrganizerName";
    REQUIRED_ATTENDEES: "required-attendees" ["calendar-required-attendees"] "Required attendees" MultiText System None "System.Calendar.RequiredAttendeeNames";
    OPTIONAL_ATTENDEES: "optional-attendees" ["calendar-optional-attendees"] "Optional attendees" MultiText System None "System.Calendar.OptionalAttendeeNames";
    RESOURCES: "resources" ["calendar-resources"] "Resources" MultiText System None "System.Calendar.Resources";
    IS_RECURRING: "is-recurring" ["recurring"] "Recurring" Bool System None "System.Calendar.IsRecurring";
    IS_ONLINE_MEETING: "is-online-meeting" ["calendar-is-online"] "Online meeting" Bool System None "System.Calendar.IsOnline";
    REMINDER_TIME: "reminder-time" ["calendar-reminder-time"] "Reminder time" Date System None "System.Calendar.ReminderTime";
    SHOW_TIME_AS: "show-time-as" ["calendar-show-time-as"] "Show time as" Int System None "System.Calendar.ShowTimeAs";
    RESPONSE_STATUS: "response-status" ["calendar-response-status"] "Response status" Int System None "System.Calendar.ResponseStatus";
    BILLING_INFORMATION: "billing-information" ["task-billing-information"] "Billing information" Text System None "System.Task.BillingInformation";
    COMPLETION_STATUS: "completion-status" ["task-completion-status"] "Completion status" Text System None "System.Task.CompletionStatus";
    TASK_OWNER: "task-owner" [] "Task owner" Text System None "System.Task.Owner";
    // media
    SUBTITLE: "subtitle" ["media-subtitle"] "Subtitle" Text Audio None "System.Media.SubTitle";
    ORIGINAL_RELEASE_YEAR: "original-release-year" ["original-year"] "Original release year" Int Audio None "";
    DATE_RELEASED: "date-released" ["release-date", "media-date-released"] "Date released" Text System None "System.Media.DateReleased";
    ENCODING_SETTINGS: "encoding-settings" ["media-encoding-settings"] "Encoding settings" Text Audio None "System.Media.EncodingSettings";
    CREATOR_APPLICATION: "creator-application" ["media-creator-application"] "Creator application" Text System None "System.Media.CreatorApplication";
    CREATOR_APPLICATION_VERSION: "creator-application-version" [] "Creator application version" Text System None "System.Media.CreatorApplicationVersion";
    CONTENT_DISTRIBUTOR: "content-distributor" ["media-content-distributor"] "Content distributor" Text System None "System.Media.ContentDistributor";
    PROVIDER_RATING: "provider-rating" ["media-provider-rating"] "Provider rating" Int System None "System.Media.ProviderRating";
    PROTECTION_TYPE: "protection-type" ["media-protection-type"] "Protection type" Text System None "System.Media.ProtectionType";
    EPISODE_NUMBER: "episode-number" ["episode"] "Episode number" Int System None "System.Media.EpisodeNumber";
    SEASON_NUMBER: "season-number" ["season"] "Season number" Int System None "System.Media.SeasonNumber";
    SERIES_NAME: "series-name" ["series"] "Series name" Text System None "System.Media.SeriesName";
    FRAME_COUNT: "frame-count" ["media-frame-count"] "Frame count" Int System None "System.Media.FrameCount";
    // music
    PART_OF_SET: "part-of-set" ["music-part-of-set"] "Part of set" Text Audio None "System.Music.PartOfSet";
    CONTENT_GROUP: "content-group-description" ["content-group", "grouping"] "Content group description" Text Audio None "System.Music.ContentGroupDescription";
    PERIOD: "period" ["music-period"] "Period" Text System None "System.Music.Period";
    DISPLAY_ARTIST: "display-artist" ["music-display-artist"] "Display artist" Text System None "System.Music.DisplayArtist";
    ALBUM_ID: "album-id" ["music-album-id"] "Album ID" Text System None "System.Music.AlbumID";
    IS_COMPILATION: "is-compilation" ["compilation"] "Compilation" Bool Audio None "System.Music.IsCompilation";
    ARTIST_SORT: "artist-sort-order" ["artist-sort-override", "artist-sort"] "Artist sort order" Text Audio None "System.Music.ArtistSortOverride";
    ALBUM_ARTIST_SORT: "album-artist-sort-order" ["album-artist-sort-override", "album-artist-sort"] "Album artist sort order" Text Audio None "System.Music.AlbumArtistSortOverride";
    COMPOSER_SORT: "composer-sort-order" ["composer-sort-override", "composer-sort"] "Composer sort order" Text Audio None "System.Music.ComposerSortOverride";
    // photo (EXIF)
    APERTURE: "aperture" ["aperture-value"] "Aperture" Float Exif None "System.Photo.Aperture";
    MAX_APERTURE: "max-aperture" ["max-aperture-value"] "Max aperture" Float Exif None "System.Photo.MaxAperture";
    SHUTTER_SPEED: "shutter-speed" ["shutter-speed-value"] "Shutter speed" Float Exif None "System.Photo.ShutterSpeed";
    BRIGHTNESS: "brightness" ["brightness-value"] "Brightness" Float Exif None "System.Photo.Brightness";
    EXPOSURE_BIAS: "exposure-bias" ["exposure-compensation"] "Exposure bias" Float Exif None "System.Photo.ExposureBias";
    EXPOSURE_INDEX: "exposure-index" [] "Exposure index" Float Exif None "System.Photo.ExposureIndex";
    EXPOSURE_PROGRAM: "exposure-program" [] "Exposure program" Int Exif None "System.Photo.ExposureProgram";
    METERING_MODE: "metering-mode" [] "Metering mode" Int Exif None "System.Photo.MeteringMode";
    WHITE_BALANCE: "white-balance" [] "White balance" Int Exif None "System.Photo.WhiteBalance";
    LIGHT_SOURCE: "light-source" [] "Light source" Int System None "System.Photo.LightSource";
    CONTRAST: "contrast" [] "Contrast" Int Exif None "System.Photo.Contrast";
    SATURATION: "saturation" [] "Saturation" Int Exif None "System.Photo.Saturation";
    SHARPNESS: "sharpness" [] "Sharpness" Int Exif None "System.Photo.Sharpness";
    GAIN_CONTROL: "gain-control" [] "Gain control" Int Exif None "System.Photo.GainControl";
    DIGITAL_ZOOM: "digital-zoom" ["digital-zoom-ratio"] "Digital zoom" Float Exif None "System.Photo.DigitalZoom";
    FOCAL_LENGTH_35MM: "focal-length-35mm" ["35mm-focal-length", "focal-length-in-film"] "35mm focal length" Int Exif Mm "System.Photo.FocalLengthInFilm";
    SUBJECT_DISTANCE: "subject-distance" [] "Subject distance" Float Exif Meters "System.Photo.SubjectDistance";
    LENS_MAKER: "lens-maker" ["lens-make", "lens-manufacturer"] "Lens maker" Text Exif None "System.Photo.LensManufacturer";
    LENS_MODEL: "lens-model" [] "Lens model" Text Exif None "System.Photo.LensModel";
    CAMERA_SERIAL_NUMBER: "camera-serial-number" ["body-serial-number"] "Camera serial number" Text Exif None "System.Photo.CameraSerialNumber";
    FLASH_MAKER: "flash-maker" ["flash-manufacturer"] "Flash maker" Text System None "System.Photo.FlashManufacturer";
    FLASH_MODEL: "flash-model" [] "Flash model" Text System None "System.Photo.FlashModel";
    FLASH_ENERGY: "flash-energy" [] "Flash energy" Float Exif None "System.Photo.FlashEnergy";
    EXIF_VERSION: "exif-version" [] "EXIF version" Text Exif None "System.Photo.EXIFVersion";
    EVENT: "event" ["photo-event"] "Event" MultiText System None "System.Photo.Event";
    FOCAL_PLANE_X_RESOLUTION: "focal-plane-x-resolution" [] "Focal plane X resolution" Float Exif None "System.Photo.FocalPlaneXResolution";
    FOCAL_PLANE_Y_RESOLUTION: "focal-plane-y-resolution" [] "Focal plane Y resolution" Float Exif None "System.Photo.FocalPlaneYResolution";
    PHOTOMETRIC_INTERPRETATION: "photometric-interpretation" [] "Photometric interpretation" Int System None "System.Photo.PhotometricInterpretation";
    PROGRAM_MODE: "program-mode" [] "Program mode" Int System None "System.Photo.ProgramMode";
    RELATED_SOUND_FILE: "related-sound-file" [] "Related sound file" Text Exif None "System.Photo.RelatedSoundFile";
    // GPS
    GPS_DATE: "gps-date" ["gps-date-stamp"] "GPS date" Date Exif None "System.GPS.Date";
    GPS_IMG_DIRECTION: "gps-img-direction" ["image-direction", "gps-image-direction"] "Image direction" Float Exif Degrees "System.GPS.ImgDirection";
    GPS_IMG_DIRECTION_REF: "gps-img-direction-ref" [] "Image direction reference" Text Exif None "System.GPS.ImgDirectionRef";
    GPS_SPEED: "gps-speed" [] "GPS speed" Float Exif None "System.GPS.Speed";
    GPS_SPEED_REF: "gps-speed-ref" [] "GPS speed unit" Text Exif None "System.GPS.SpeedRef";
    GPS_TRACK: "gps-track" [] "GPS track" Float Exif Degrees "System.GPS.Track";
    GPS_TRACK_REF: "gps-track-ref" [] "GPS track reference" Text Exif None "System.GPS.TrackRef";
    GPS_DOP: "gps-dop" [] "GPS DOP" Float Exif None "System.GPS.DOP";
    GPS_SATELLITES: "gps-satellites" [] "GPS satellites" Text Exif None "System.GPS.Satellites";
    GPS_STATUS: "gps-status" [] "GPS status" Text Exif None "System.GPS.Status";
    GPS_MEASURE_MODE: "gps-measure-mode" [] "GPS measure mode" Text Exif None "System.GPS.MeasureMode";
    GPS_MAP_DATUM: "gps-map-datum" [] "GPS map datum" Text Exif None "System.GPS.MapDatum";
    GPS_PROCESSING_METHOD: "gps-processing-method" [] "GPS processing method" Text System None "System.GPS.ProcessingMethod";
    GPS_AREA_INFORMATION: "gps-area-information" [] "GPS area information" Text System None "System.GPS.AreaInformation";
    GPS_DEST_LATITUDE: "gps-dest-latitude" ["destination-latitude"] "Destination latitude" Float Exif Degrees "System.GPS.DestLatitude";
    GPS_DEST_LONGITUDE: "gps-dest-longitude" ["destination-longitude"] "Destination longitude" Float Exif Degrees "System.GPS.DestLongitude";
    GPS_DEST_BEARING: "gps-dest-bearing" ["destination-bearing"] "Destination bearing" Float Exif Degrees "System.GPS.DestBearing";
    GPS_DEST_DISTANCE: "gps-dest-distance" ["destination-distance"] "Destination distance" Float Exif None "System.GPS.DestDistance";
    GPS_DIFFERENTIAL: "gps-differential" [] "GPS differential" Int Exif None "System.GPS.Differential";
    // image
    COLOR_SPACE: "color-space" ["colour-space"] "Color representation" Int Exif None "System.Image.ColorSpace";
    COMPRESSED_BITS_PER_PIXEL: "compressed-bits-per-pixel" [] "Compressed bits/pixel" Float Exif None "System.Image.CompressedBitsPerPixel";
    IMAGE_COMPRESSION: "image-compression" [] "Image compression" Int Exif None "System.Image.Compression";
    IMAGE_ID: "image-id" ["image-unique-id"] "Image ID" Text Exif None "System.Image.ImageID";
    RESOLUTION_UNIT: "resolution-unit" [] "Resolution unit" Int Exif None "System.Image.ResolutionUnit";
    // video
    HORIZONTAL_ASPECT_RATIO: "horizontal-aspect-ratio" [] "Horizontal aspect ratio" Int System None "System.Video.HorizontalAspectRatio";
    VERTICAL_ASPECT_RATIO: "vertical-aspect-ratio" [] "Vertical aspect ratio" Int System None "System.Video.VerticalAspectRatio";
    VIDEO_ORIENTATION: "video-orientation" [] "Video orientation" Int System None "System.Video.Orientation";
    VIDEO_SAMPLE_SIZE: "video-sample-size" [] "Video sample size" Int System Bits "System.Video.SampleSize";
    VIDEO_STREAM_NAME: "video-stream-name" [] "Video stream name" Text System None "System.Video.StreamName";
    VIDEO_STREAM_NUMBER: "video-stream-number" [] "Video stream number" Int System None "System.Video.StreamNumber";
    IS_STEREO: "is-stereo" ["stereo"] "Stereo" Bool System None "System.Video.IsStereo";
    IS_SPHERICAL: "is-spherical" ["spherical"] "Spherical" Bool System None "System.Video.IsSpherical";
    // audio
    AUDIO_FORMAT: "audio-format" [] "Audio format" Text System None "System.Audio.Format";
    AUDIO_COMPRESSION: "audio-compression" [] "Audio compression" Text System None "System.Audio.Compression";
    PEAK_VALUE: "peak-value" [] "Peak value" Int System None "System.Audio.PeakValue";
    AUDIO_STREAM_NAME: "audio-stream-name" [] "Audio stream name" Text System None "System.Audio.StreamName";
    AUDIO_STREAM_NUMBER: "audio-stream-number" [] "Audio stream number" Int System None "System.Audio.StreamNumber";
    // document
    CONTRIBUTORS: "contributors" ["contributor"] "Contributors" MultiText System None "System.Document.Contributor";
    DIVISION: "division" [] "Division" Text System None "System.Document.Division";
    DOCUMENT_ID: "document-id" [] "Document ID" Text System None "System.Document.DocumentID";
    CLIENT_ID: "client-id" [] "Client ID" Text System None "System.Document.ClientID";
    DOCUMENT_VERSION: "document-version" [] "Version number" Text Document None "System.Document.Version";
    BYTE_COUNT: "byte-count" [] "Byte count" Int System None "System.Document.ByteCount";
    HIDDEN_SLIDE_COUNT: "hidden-slide-count" ["hidden-slides"] "Hidden slides" Int Document None "System.Document.HiddenSlideCount";
    NOTE_COUNT: "note-count" ["notes"] "Notes" Int Document None "System.Document.NoteCount";
    MULTIMEDIA_CLIP_COUNT: "multimedia-clip-count" ["multimedia-clips"] "Multimedia clips" Int Document None "System.Document.MultimediaClipCount";
    PRESENTATION_FORMAT: "presentation-format" [] "Presentation format" Text Document None "System.Document.PresentationFormat";
    // shortcuts and links
    URL: "url" ["target-url", "link-target-url"] "URL" Text System None "System.Link.TargetUrl";
    LINK_ARGUMENTS: "link-arguments" ["arguments"] "Arguments" Text System None "System.Link.Arguments";
    LINK_COMMENT: "link-comment" [] "Link comment" Text System None "System.Link.Comment";
    LINK_DESCRIPTION: "link-description" [] "Link description" Text System None "System.Link.Description";
    LINK_STATUS: "link-status" [] "Link status" Int System None "System.Link.Status";
    LINK_TARGET_EXTENSION: "link-target-extension" [] "Link target extension" MultiText System None "System.Link.TargetExtension";
    DATE_VISITED: "date-visited" [] "Date visited" Date System None "System.Link.DateVisited";
}

/// Normalises a search name: ASCII lowercase, `-`/`_` removed (like function names).
pub fn normalize(name: &str) -> String {
    name.chars().filter(|c| *c != '-' && *c != '_').map(|c| c.to_ascii_lowercase()).collect()
}

/// Normalised search name → property (built once; lookups are O(1), see [`PropertyId::lookup`]).
static BY_NAME: LazyLock<HashMap<Box<str>, PropertyId>> = LazyLock::new(|| {
    let mut m = HashMap::with_capacity(PROPERTIES.len() * 3);
    for d in PROPERTIES {
        for n in std::iter::once(&d.canonical).chain(d.aliases) {
            m.entry(normalize(n).into_boxed_str()).or_insert(d.id);
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
    /// Allocation-free for names that are already normalised (lowercase, no `-`/`_`).
    pub fn lookup(name: &str) -> Option<PropertyId> {
        if name.bytes().any(|b| b == b'-' || b == b'_' || b.is_ascii_uppercase()) {
            BY_NAME.get(normalize(name).as_str()).copied()
        } else {
            BY_NAME.get(name).copied()
        }
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
            // Windows `PERCEIVED_TYPE_*` (shtypes.h).
            PropertyId::PERCEIVED_TYPE => &[
                ("folder", -3),
                ("custom", -2),
                ("unspecified", -1),
                ("unknown-type", 0),
                ("text", 1),
                ("image", 2),
                ("audio", 3),
                ("video", 4),
                ("compressed", 5),
                ("document", 6),
                ("system", 7),
                ("application", 8),
                ("game-media", 9),
                ("contacts", 10),
            ],
            // EXIF values; Windows' `PHOTO_*` constants use the same numbers.
            PropertyId::EXPOSURE_PROGRAM => &[
                ("not-defined", 0),
                ("manual", 1),
                ("normal", 2),
                ("aperture-priority", 3),
                ("shutter-priority", 4),
                ("creative", 5),
                ("action", 6),
                ("portrait", 7),
                ("landscape", 8),
            ],
            PropertyId::METERING_MODE => &[
                ("average", 1),
                ("center-weighted-average", 2),
                ("center-weighted", 2),
                ("spot", 3),
                ("multi-spot", 4),
                ("pattern", 5),
                ("partial", 6),
                ("other", 255),
            ],
            PropertyId::WHITE_BALANCE => &[("auto", 0), ("manual", 1)],
            PropertyId::CONTRAST | PropertyId::SHARPNESS => &[("normal", 0), ("soft", 1), ("hard", 2)],
            PropertyId::SATURATION => &[("normal", 0), ("low", 1), ("high", 2)],
            PropertyId::GAIN_CONTROL => {
                &[("none", 0), ("low-gain-up", 1), ("high-gain-up", 2), ("low-gain-down", 3), ("high-gain-down", 4)]
            }
            PropertyId::COLOR_SPACE => &[("srgb", 1), ("uncalibrated", 0xffff)],
            PropertyId::RESOLUTION_UNIT => &[("none", 1), ("inches", 2), ("centimeters", 3)],
            PropertyId::IMAGE_COMPRESSION => &[
                ("uncompressed", 1),
                ("ccitt-t3", 2),
                ("ccitt-t4", 3),
                ("ccitt-t6", 4),
                ("lzw", 5),
                ("jpeg", 6),
                ("packbits", 32773),
            ],
            PropertyId::FLAG_STATUS => &[("not-flagged", 0), ("completed", 1), ("follow-up", 2)],
            PropertyId::FLAG_COLOR => {
                &[("purple", 1), ("orange", 2), ("green", 3), ("yellow", 4), ("blue", 5), ("red", 6)]
            }
            PropertyId::OFFLINE_AVAILABILITY => &[("not-available", 0), ("available", 1), ("always-available", 2)],
            PropertyId::OFFLINE_STATUS => &[
                ("online", 0),
                ("offline", 1),
                ("offline-forced", 2),
                ("offline-slow", 3),
                ("offline-error", 4),
                ("offline-item-version-conflict", 5),
                ("offline-suspended", 6),
            ],
            PropertyId::SHARING_STATUS => &[("not-shared", 0), ("shared", 1), ("private", 2)],
            PropertyId::LINK_STATUS => &[("unresolved", 1), ("broken", 2)],
            PropertyId::SHOW_TIME_AS => &[("free", 0), ("tentative", 1), ("busy", 2), ("out-of-office", 3)],
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
                assert_eq!(PropertyId::lookup(n), Some(d.id), "{n}");
            }
            assert!(!d.title.is_empty());
        }
        // SPEC ch.2 appendix B: about 300 properties.
        assert!((290..=330).contains(&PropertyId::count()), "{}", PropertyId::count());
        assert_eq!(PROPERTIES.len(), PropertyId::all().count());
    }

    #[test]
    fn table_invariants() {
        let mut systems = std::collections::HashSet::new();
        for d in PROPERTIES {
            // Everything-style names: lowercase ASCII words joined by single dashes.
            for n in std::iter::once(&d.canonical).chain(d.aliases) {
                assert!(
                    !n.is_empty()
                        && n.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                        && !n.starts_with('-')
                        && !n.ends_with('-')
                        && !n.contains("--"),
                    "bad search name {n:?}"
                );
            }
            assert!(!d.aliases.contains(&d.canonical), "{} repeats its canonical name", d.canonical);
            // Properties only the Windows provider can read need their `System.*` name;
            // the others either have one or come from a built-in parser.
            if d.extractor == Extractor::System {
                assert!(!d.system.is_empty(), "{} has no System.* name", d.canonical);
            }
            if !d.system.is_empty() {
                assert!(
                    d.system.starts_with("System.")
                        && d.system
                            .split('.')
                            .skip(1)
                            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric())),
                    "bad Windows name {}",
                    d.system
                );
                assert!(systems.insert(d.system), "{} used twice", d.system);
            }
            assert!(d.title.trim() == d.title && !d.title.is_empty(), "{}", d.canonical);
            // Units only on numeric types; booleans/enumerations have no unit.
            if d.unit != Unit::None {
                assert!(matches!(d.ty, PropType::Int | PropType::Float), "{} unit on {:?}", d.canonical, d.ty);
            }
            // Named values only on integers.
            if !d.id.value_names().is_empty() {
                assert_eq!(d.ty, PropType::Int, "{}", d.canonical);
            }
        }
        // Built-in-only entries are few and deliberate.
        let builtin_only: Vec<&str> = PROPERTIES.iter().filter(|d| d.system.is_empty()).map(|d| d.canonical).collect();
        assert!(builtin_only.len() <= 20, "{builtin_only:?}");
    }

    #[test]
    fn appendix_b_names() {
        let names = "album-artist camera-maker date-taken subject comments copyright keywords tags company \
            manager category content-status revision-number total-editing-time slide-count word-count \
            character-count sender to cc message-date-received contact-first-name contact-email \
            gps-latitude gps-img-direction exposure-bias exposure-program iso-speed flash white-balance \
            metering-mode lens-maker lens-model video-orientation audio-format encoded-by publisher \
            original-release-year bpm composer conductor mood part-of-set rating perceived-type kind-text \
            owner computer file-description product-version url offline-status sharing-status \
            35mm-focal-length focal-length-35mm from bcc date-sent has-attachments location organizer";
        for n in names.split_whitespace() {
            assert!(PropertyId::lookup(n).is_some(), "{n} not registered");
        }
        assert_eq!(PropertyId::lookup("message-date-received"), Some(PropertyId::DATE_RECEIVED));
        assert_eq!(PropertyId::lookup("lens-manufacturer"), Some(PropertyId::LENS_MAKER));
        assert_eq!(PropertyId::lookup("LensManufacturer"), Some(PropertyId::LENS_MAKER));
        assert_eq!(PropertyId::lookup("contactfirstname"), Some(PropertyId::FIRST_NAME));
        assert_eq!(PropertyId::DATE_RECEIVED.ty(), PropType::Date);
        assert_eq!(PropertyId::TO.ty(), PropType::MultiText);
        assert_eq!(PropertyId::HAS_ATTACHMENTS.ty(), PropType::Bool);
        assert_eq!(PropertyId::FOCAL_LENGTH_35MM.unit(), Unit::Mm);
        assert_eq!(PropertyId::OWNER.extractor(), Extractor::System);
        assert_eq!(PropertyId::PERCEIVED_TYPE.named_value("image"), Some(2));
        assert_eq!(PropertyId::METERING_MODE.named_value("Spot"), Some(3));
        assert_eq!(PropertyId::OFFLINE_STATUS.named_value("offline"), Some(1));
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
