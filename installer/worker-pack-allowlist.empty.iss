// CPU-only Stage 4 releases intentionally ship an empty worker-pack catalog.
function IsGeneratedWorkerPackDirectory(RelativePath: String): Boolean;
begin
  Result := False;
end;

function IsGeneratedWorkerPackFile(RelativePath: String): Boolean;
begin
  Result := False;
end;

function GetGeneratedCurrentWorkerPackFileIdentity(
  RelativePath: String; var FileSize: Int64; var Sha256: String
): Boolean;
begin
  FileSize := -1;
  Sha256 := '';
  Result := False;
end;

function GetGeneratedCurrentWorkerPackFileCount(): Integer;
begin
  Result := 0;
end;

{ The checked-in include must never grant catalog authority. Release staging
  replaces it with identities derived from verified current and historical
  catalogs. }
function IsGeneratedCurrentWorkerCatalog(FileSize: Int64; Sha256: String): Boolean;
begin
  Result := False;
end;

function IsGeneratedKnownWorkerCatalog(FileSize: Int64; Sha256: String): Boolean;
begin
  Result := False;
end;
