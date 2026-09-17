use super::{Field, ID, Tool, Ty, WS};

pub(super) const ATTACH: Tool = Tool {
    name: "ntk_attach",
    cli: "",
    title: "Attach a file",
    about: "Upload a file and attach it to a ticket.",
    desc: "Upload a file to an existing ticket in one call. Provide exactly one source: content for UTF-8 text, content_base64 for binary files, or path for an existing file on the LOCAL stdio MCP machine. Remote HTTP MCP cannot read local paths: send content or content_base64 instead. File size: 1 byte to 50 MiB; remote JSON request limit is about 68 MiB. The upload is verified in storage before the attachment is recorded. No tracker URL or browser form is needed: workspace and ticket id are sufficient. Use ntk_attachments to list files and get download links. A repeated call creates another attachment; after an uncertain result, list attachments before retrying.",
    read_only: false,
    destructive: false,
    idempotent: false,
    fields: &[
        WS,
        ID,
        Field::req(
            "filename",
            Ty::Str,
            "Displayed filename, including extension; 1 to 256 characters.",
        ),
        Field::opt(
            "content_type",
            Ty::Str,
            "MIME type, for example text/markdown or application/pdf. Defaults to application/octet-stream.",
        ),
        Field::opt(
            "content",
            Ty::Str,
            "UTF-8 file contents. Exactly one of content, content_base64 or path is required.",
        ),
        Field::opt(
            "content_base64",
            Ty::Str,
            "Standard padded base64 of the file bytes; no data URL prefix. Use for binary files.",
        ),
        Field::opt(
            "path",
            Ty::Str,
            "Absolute path to an existing file. LOCAL stdio MCP only; remote MCP must receive content or content_base64.",
        ),
    ],
};

pub(super) const LIST: Tool = Tool {
    name: "ntk_attachments",
    cli: "",
    title: "List ticket attachments",
    about: "List attached files and get temporary download links.",
    desc: "List files attached to a ticket, with filenames, MIME types, sizes, upload times and signed download URLs valid for 15 minutes. Call again to refresh expired links. Use ntk_attach to upload files; no tracker URL or browser form is needed.",
    read_only: true,
    destructive: false,
    idempotent: true,
    fields: &[WS, ID],
};
