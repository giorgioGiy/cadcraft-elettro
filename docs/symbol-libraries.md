# Local symbol libraries

CADCraft desktop can browse a local ZIP catalog containing one DXF or DWG drawing per symbol.
Open **Insert ▸ Symbol Library…** or type `SIMBOLI`, choose the ZIP, search by file name, and
double-click a preview. CADCraft then starts the ordinary `INSERT` command so the insertion point
is still chosen on the drawing. The insertion-scale field is sent to `INSERT` before that point.

ZIP folders are allowed. The drawing file's stem becomes the symbol code: for example,
`planimetria/PRESA.DWG` appears as `PRESA`. Non-DXF/DWG entries are ignored. If the same code
appears more than once, only the first indexed entry is shown.

The selected ZIP path is kept in the user preferences and reopened at the next start. A script or
the control channel can load a catalog without opening a file picker:

```text
simboli {"path":"C:\\symbols\\electrical.zip"}
```

The library remains external to the drawing until a symbol is inserted. At insertion, CADCraft
reads that single entry, defines it as a normal block, and removes the temporary file. Archive
entries are never extracted using their stored paths. Archives are capped at 10,000 entries and
each expanded drawing at 32 MiB.

## Licensing

Only use symbol files that you are permitted to use and redistribute. Proprietary catalogs can
stay local and work with the browser, but must not be committed to the CADCraft repository or
included in public releases. A public CADCraft symbol pack needs its own redistributable licence
and attribution.
