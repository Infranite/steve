use crate::{Read, Result};
use sea_query::{Expr, ExprTrait, Query};

pub type FileInfo = (String, String, String, String, i64);

pub fn file_row(c: &mut impl Read, id: &str) -> Result<Option<FileInfo>> {
    c.one(
        Query::select()
            .columns(["workspace_id", "sha256", "name", "mime", "size"])
            .from("files")
            .and_where(Expr::col("id").eq(id)),
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )
}
