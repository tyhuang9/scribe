// Stage 3 production releases intentionally ship an empty worker-pack catalog.
function IsGeneratedWorkerPackDirectory(RelativePath: String): Boolean;
begin
  Result := False;
end;

function IsGeneratedWorkerPackFile(RelativePath: String): Boolean;
begin
  Result := False;
end;
