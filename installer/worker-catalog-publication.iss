{ Transactional publication for worker-pack-catalog.json.

  The catalog is runtime authority, so Inno never writes it in place. The
  payload is staged as worker-pack-catalog.next.json and this code publishes
  it only after the complete [Files] phase has succeeded. All three recovery
  names are exact and bounded; no wildcard cleanup is permitted here. }

type
  TByHandleFileInformation = record
    FileAttributes: LongWord;
    CreationTime: TNativeFileTime;
    LastAccessTime: TNativeFileTime;
    LastWriteTime: TNativeFileTime;
    VolumeSerialNumber: LongWord;
    FileSizeHigh: LongWord;
    FileSizeLow: LongWord;
    NumberOfLinks: LongWord;
    FileIndexHigh: LongWord;
    FileIndexLow: LongWord;
  end;
  TFileRenameInformation = record
    ReplaceIfExists: LongWord;
    RootDirectory: THandle;
    FileNameLength: LongWord;
    FileName: array[0..259] of Char;
  end;
  TFileDispositionInformation = record
    DeleteFile: LongWord;
  end;
  TWorkerCatalogKind = (wckAbsent, wckCurrent, wckHistorical);
  TWorkerCatalogMode = (
    wcmUninitialized,
    wcmFreshStage,
    wcmUpdateStage,
    wcmMissingLiveRestage,
    wcmCommittedStage,
    wcmRecoverFresh,
    wcmRecoverUpdateBeforeFirstRename,
    wcmRecoverUpdateBeforeSecondRename,
    wcmRepairRedundantNext
  );
  TWorkerCatalogLease = record
    Handle: THandle;
    Path: String;
    Present: Boolean;
    Kind: TWorkerCatalogKind;
    FileSize: Int64;
    Sha256: String;
    Information: TByHandleFileInformation;
  end;

const
  DeleteAccess = $00010000;
  FileRenameInfo = 3;
  FileDispositionInfo = 4;
  WorkerCatalogLiveName = 'worker-pack-catalog.json';
  WorkerCatalogNextName = 'worker-pack-catalog.next.json';
  WorkerCatalogPreviousName = 'worker-pack-catalog.previous.json';
  WorkerCatalogFailureExitCode = 73;
  MaxWorkerCatalogBytes = 524288;

var
  WorkerCatalogLeases: array[0..2] of TWorkerCatalogLease;
  WorkerCatalogRoot: String;
  WorkerCatalogMode: TWorkerCatalogMode;
  WorkerCatalogStageRequired: Boolean;
  WorkerCatalogLifecycleSuccessful: Boolean;
  WorkerCatalogLifecycleExitCode: Integer;
  ObservedCurrentWorkerPackFileCount: Integer;

function SetFileRenameInformationByHandle(
  FileHandle: THandle;
  FileInformationClass: LongWord;
  var FileInformation: TFileRenameInformation;
  BufferSize: LongWord
): Boolean;
  external 'SetFileInformationByHandle@kernel32.dll stdcall';

function SetFileDispositionInformationByHandle(
  FileHandle: THandle;
  FileInformationClass: LongWord;
  var FileInformation: TFileDispositionInformation;
  BufferSize: LongWord
): Boolean;
  external 'SetFileInformationByHandle@kernel32.dll stdcall';

function GetFileInformationByHandle(
  FileHandle: THandle;
  var FileInformation: TByHandleFileInformation
): Boolean;
  external 'GetFileInformationByHandle@kernel32.dll stdcall';

function IsCatalogRecoveryRelativePath(RelativePath: String): Boolean;
begin
  Result :=
    SameStr(RelativePath, WorkerCatalogLiveName) or
    SameStr(RelativePath, WorkerCatalogNextName) or
    SameStr(RelativePath, WorkerCatalogPreviousName);
end;

function SameFileIdentity(
  const Left: TByHandleFileInformation;
  const Right: TByHandleFileInformation
): Boolean;
begin
  Result :=
    (Left.VolumeSerialNumber = Right.VolumeSerialNumber) and
    (Left.FileIndexHigh = Right.FileIndexHigh) and
    (Left.FileIndexLow = Right.FileIndexLow);
end;

function SameFileSnapshot(
  const Left: TByHandleFileInformation;
  const Right: TByHandleFileInformation
): Boolean;
begin
  Result :=
    SameFileIdentity(Left, Right) and
    (Left.FileAttributes = Right.FileAttributes) and
    (Left.FileSizeHigh = Right.FileSizeHigh) and
    (Left.FileSizeLow = Right.FileSizeLow) and
    (Left.NumberOfLinks = Right.NumberOfLinks);
end;

function FileInformationSize(const Information: TByHandleFileInformation): Int64;
begin
  Result :=
    (Int64(Information.FileSizeHigh) * 4294967296) +
    Int64(Information.FileSizeLow);
end;

function IsSafeRegularSingleLinkFile(
  const Path: String;
  const Information: TByHandleFileInformation;
  var ErrorText: String
): Boolean;
begin
  Result := False;
  if ((Information.FileAttributes and FILE_ATTRIBUTE_DIRECTORY) <> 0) or
     ((Information.FileAttributes and FILE_ATTRIBUTE_REPARSE_POINT) <> 0) or
     ((Information.FileAttributes and FILE_ATTRIBUTE_DEVICE) <> 0) then
  begin
    ErrorText := 'Scribe Setup refused a catalog-controlled path that is not a regular no-follow file: ' + Path;
    Exit;
  end;
  if Information.NumberOfLinks <> 1 then
  begin
    ErrorText := 'Scribe Setup refused a catalog-controlled file with more than one hard link: ' + Path;
    Exit;
  end;
  if not RejectAlternateStreams(Path, False, ErrorText) then
    Exit;
  Result := True;
end;

procedure ClearWorkerCatalogLease(Index: Integer; CloseLease: Boolean);
begin
  if CloseLease and (WorkerCatalogLeases[Index].Handle <> InvalidHandleValue) then
    CloseHandle(WorkerCatalogLeases[Index].Handle);
  WorkerCatalogLeases[Index].Handle := InvalidHandleValue;
  WorkerCatalogLeases[Index].Path := '';
  WorkerCatalogLeases[Index].Present := False;
  WorkerCatalogLeases[Index].Kind := wckAbsent;
  WorkerCatalogLeases[Index].FileSize := -1;
  WorkerCatalogLeases[Index].Sha256 := '';
end;

procedure ReleaseWorkerCatalogLeases();
var
  Index: Integer;
begin
  for Index := 0 to 2 do
    ClearWorkerCatalogLease(Index, True);
end;

function OpenWorkerCatalogLease(
  Index: Integer;
  const Path: String;
  var ErrorText: String
): Boolean;
var
  Attributes: LongWord;
  PathExists: Boolean;
  FileHandle: THandle;
  BeforeInformation: TByHandleFileInformation;
  AfterInformation: TByHandleFileInformation;
  FileSize: Int64;
  Sha256: String;
  ErrorCode: LongInt;
begin
  Result := False;
  ClearWorkerCatalogLease(Index, True);
  WorkerCatalogLeases[Index].Path := Path;
  if not QueryExistingAttributes(Path, Attributes, PathExists, ErrorText) then
    Exit;
  if not PathExists then
  begin
    WorkerCatalogLeases[Index].Kind := wckAbsent;
    Result := True;
    Exit;
  end;

  { A read/share-read lease is compatible with Inno's path-based SHA-256
    reader while excluding existing and later writers or deleters. Mutation
    upgrades this lease to DELETE access and verifies the retained file ID. }
  FileHandle := CreateFileW(
    Path, GenericRead, FileShareRead, 0, OpenExisting,
    FileFlagOpenReparsePoint, 0);
  if FileHandle = InvalidHandleValue then
  begin
    ErrorCode := DLLGetLastError;
    ErrorText := 'Scribe Setup could not obtain an exclusive catalog publication lease: ' +
      Path + ' (' + SysErrorMessage(ErrorCode) + '). Close Scribe and retry.';
    Exit;
  end;
  WorkerCatalogLeases[Index].Handle := FileHandle;
  WorkerCatalogLeases[Index].Present := True;
  if not GetFileInformationByHandle(FileHandle, BeforeInformation) then
  begin
    ErrorCode := DLLGetLastError;
    ErrorText := 'Scribe Setup could not read catalog file identity: ' +
      Path + ' (' + SysErrorMessage(ErrorCode) + ').';
    Exit;
  end;
  if not IsSafeRegularSingleLinkFile(Path, BeforeInformation, ErrorText) then
    Exit;
  FileSize := FileInformationSize(BeforeInformation);
  if (FileSize < 0) or (FileSize > MaxWorkerCatalogBytes) then
  begin
    ErrorText := 'Scribe Setup refused a worker-pack catalog outside the 512 KiB authentication bound: ' + Path;
    Exit;
  end;
  Sha256 := Lowercase(GetSHA256OfFile(Path));
  if not GetFileInformationByHandle(FileHandle, AfterInformation) then
  begin
    ErrorCode := DLLGetLastError;
    ErrorText := 'Scribe Setup could not revalidate catalog file identity: ' +
      Path + ' (' + SysErrorMessage(ErrorCode) + ').';
    Exit;
  end;
  if not SameFileSnapshot(BeforeInformation, AfterInformation) then
  begin
    ErrorText := 'Scribe Setup refused a catalog file that changed during authentication: ' + Path;
    Exit;
  end;
  if IsGeneratedCurrentWorkerCatalog(FileSize, Sha256) then
    WorkerCatalogLeases[Index].Kind := wckCurrent
  else if IsGeneratedKnownWorkerCatalog(FileSize, Sha256) then
    WorkerCatalogLeases[Index].Kind := wckHistorical
  else
  begin
    ErrorText := 'Scribe Setup refused an unknown or corrupt worker-pack catalog: ' + Path;
    Exit;
  end;
  WorkerCatalogLeases[Index].FileSize := FileSize;
  WorkerCatalogLeases[Index].Sha256 := Sha256;
  WorkerCatalogLeases[Index].Information := AfterInformation;
  Result := True;
end;

function ReopenAndMatchWorkerCatalogLease(
  Index: Integer;
  var ErrorText: String
): Boolean;
  forward;

function UpgradeWorkerCatalogLeaseForMutation(
  Index: Integer;
  var ErrorText: String
): Boolean;
var
  RetainedInformation: TByHandleFileInformation;
  MutationInformation: TByHandleFileInformation;
  MutationHandle: THandle;
  ErrorCode: LongInt;
begin
  Result := False;
  if not ReopenAndMatchWorkerCatalogLease(Index, ErrorText) then
    Exit;
  RetainedInformation := WorkerCatalogLeases[Index].Information;
  if not CloseHandle(WorkerCatalogLeases[Index].Handle) then
  begin
    ErrorCode := DLLGetLastError;
    WorkerCatalogLeases[Index].Handle := InvalidHandleValue;
    ErrorText := 'Scribe Setup could not close a catalog authentication lease before mutation (' +
      SysErrorMessage(ErrorCode) + ').';
    Exit;
  end;
  WorkerCatalogLeases[Index].Handle := InvalidHandleValue;
  MutationHandle := CreateFileW(
    WorkerCatalogLeases[Index].Path,
    GenericRead or DeleteAccess,
    FileShareRead,
    0,
    OpenExisting,
    FileFlagOpenReparsePoint,
    0);
  if MutationHandle = InvalidHandleValue then
  begin
    ErrorCode := DLLGetLastError;
    ErrorText := 'Scribe Setup could not upgrade the retained catalog lease for mutation (' +
      SysErrorMessage(ErrorCode) + ').';
    Exit;
  end;
  WorkerCatalogLeases[Index].Handle := MutationHandle;
  if not GetFileInformationByHandle(MutationHandle, MutationInformation) or
     not SameFileSnapshot(RetainedInformation, MutationInformation) then
  begin
    ErrorText := 'Scribe Setup refused a catalog file whose identity changed while upgrading its retained lease.';
    Exit;
  end;
  WorkerCatalogLeases[Index].Information := MutationInformation;
  Result := True;
end;

function RequireCatalogPathAbsent(const Path: String; var ErrorText: String): Boolean;
var
  Attributes: LongWord;
  PathExists: Boolean;
begin
  Result := QueryExistingAttributes(Path, Attributes, PathExists, ErrorText);
  if Result and PathExists then
  begin
    ErrorText := 'Scribe Setup refused a catalog recovery path that became occupied: ' + Path;
    Result := False;
  end;
end;

function ReopenAndMatchWorkerCatalogLease(
  Index: Integer;
  var ErrorText: String
): Boolean;
var
  ReopenedHandle: THandle;
  Information: TByHandleFileInformation;
  FileSize: Int64;
  Sha256: String;
  ErrorCode: LongInt;
begin
  Result := False;
  if not WorkerCatalogLeases[Index].Present or
     (WorkerCatalogLeases[Index].Handle = InvalidHandleValue) then
  begin
    ErrorText := 'Scribe Setup lost a required catalog publication lease.';
    Exit;
  end;
  ReopenedHandle := CreateFileW(
    WorkerCatalogLeases[Index].Path, GenericRead, FileShareRead, 0,
    OpenExisting, FileFlagOpenReparsePoint, 0);
  if ReopenedHandle = InvalidHandleValue then
  begin
    ErrorCode := DLLGetLastError;
    ErrorText := 'Scribe Setup could not reopen a leased catalog path: ' +
      WorkerCatalogLeases[Index].Path + ' (' + SysErrorMessage(ErrorCode) + ').';
    Exit;
  end;
  try
    if not GetFileInformationByHandle(ReopenedHandle, Information) or
       not SameFileIdentity(WorkerCatalogLeases[Index].Information, Information) then
    begin
      ErrorText := 'Scribe Setup refused a catalog path whose file identity changed.';
      Exit;
    end;
    if not IsSafeRegularSingleLinkFile(
      WorkerCatalogLeases[Index].Path, Information, ErrorText) then
      Exit;
    FileSize := FileInformationSize(Information);
    Sha256 := Lowercase(GetSHA256OfFile(WorkerCatalogLeases[Index].Path));
    if (FileSize <> WorkerCatalogLeases[Index].FileSize) or
       not SameStr(Sha256, WorkerCatalogLeases[Index].Sha256) then
    begin
      ErrorText := 'Scribe Setup refused a catalog path whose authenticated bytes changed.';
      Exit;
    end;
  finally
    CloseHandle(ReopenedHandle);
  end;
  Result := True;
end;

function VerifyAndBindCurrentWorkerPackFile(
  const Path: String;
  const RelativePath: String;
  var ErrorText: String
): Boolean;
var
  ExpectedSize: Int64;
  ExpectedSha256: String;
  FileHandle: THandle;
  BeforeInformation: TByHandleFileInformation;
  AfterInformation: TByHandleFileInformation;
  ErrorCode: LongInt;
begin
  Result := False;
  if not GetGeneratedCurrentWorkerPackFileIdentity(
    RelativePath, ExpectedSize, ExpectedSha256) then
  begin
    ErrorText := 'Scribe Setup refused a current worker-pack path without an exact generated identity: ' + RelativePath;
    Exit;
  end;
  FileHandle := CreateFileW(
    Path, GenericRead, FileShareRead, 0, OpenExisting,
    FileFlagOpenReparsePoint, 0);
  if FileHandle = InvalidHandleValue then
  begin
    ErrorCode := DLLGetLastError;
    ErrorText := 'Scribe Setup could not lease a current worker-pack file: ' +
      Path + ' (' + SysErrorMessage(ErrorCode) + '). Close Scribe and retry.';
    Exit;
  end;
  if not RetainBoundHandle(FileHandle, Path, False, False, ErrorText) then
    Exit;
  if not GetFileInformationByHandle(FileHandle, BeforeInformation) then
  begin
    ErrorCode := DLLGetLastError;
    ErrorText := 'Scribe Setup could not read current worker-pack file identity: ' +
      Path + ' (' + SysErrorMessage(ErrorCode) + ').';
    Exit;
  end;
  if not IsSafeRegularSingleLinkFile(Path, BeforeInformation, ErrorText) then
    Exit;
  if (FileInformationSize(BeforeInformation) <> ExpectedSize) or
     not SameText(GetSHA256OfFile(Path), ExpectedSha256) then
  begin
    ErrorText := 'Scribe Setup refused a current worker-pack file whose bytes do not match the generated identity: ' + RelativePath;
    Exit;
  end;
  if not GetFileInformationByHandle(FileHandle, AfterInformation) or
     not SameFileSnapshot(BeforeInformation, AfterInformation) then
  begin
    ErrorText := 'Scribe Setup refused a current worker-pack file that changed during authentication: ' + RelativePath;
    Exit;
  end;
  Result := True;
end;

function CurrentWorkerPackPayloadIsComplete(var ErrorText: String): Boolean;
var
  ExpectedCount: Integer;
begin
  ExpectedCount := GetGeneratedCurrentWorkerPackFileCount();
  Result := (ExpectedCount >= 0) and (ExpectedCount <= 1024) and
    (ObservedCurrentWorkerPackFileCount = ExpectedCount);
  if not Result then
    ErrorText := 'Scribe Setup refused to publish the worker-pack catalog because the complete current worker payload was not authenticated.';
end;

function CatalogPath(Index: Integer): String;
begin
  if Index = 0 then
    Result := AddBackslash(WorkerCatalogRoot) + WorkerCatalogLiveName
  else if Index = 1 then
    Result := AddBackslash(WorkerCatalogRoot) + WorkerCatalogNextName
  else
    Result := AddBackslash(WorkerCatalogRoot) + WorkerCatalogPreviousName;
end;

function PrepareWorkerCatalogPublication(
  const InstallRoot: String;
  var ErrorText: String
): Boolean;
var
  Index: Integer;
begin
  Result := False;
  ReleaseWorkerCatalogLeases();
  WorkerCatalogMode := wcmUninitialized;
  WorkerCatalogStageRequired := False;
  WorkerCatalogRoot := RemoveBackslashUnlessRoot(ExpandFileName(InstallRoot));
  for Index := 0 to 2 do
    if Length(CatalogPath(Index)) > 260 then
    begin
      ErrorText := 'Scribe Setup refused an install path too long for proven catalog publication.';
      Exit;
    end;
  for Index := 0 to 2 do
    if not OpenWorkerCatalogLease(Index, CatalogPath(Index), ErrorText) then
      Exit;

  { next may only ever contain the current B catalog. previous is preserved A
    authority and is intentionally not retired in this phase. }
  if WorkerCatalogLeases[1].Present and
     (WorkerCatalogLeases[1].Kind <> wckCurrent) then
  begin
    ErrorText := 'Scribe Setup refused a non-current staged worker-pack catalog.';
    Exit;
  end;
  if WorkerCatalogLeases[2].Present and
     (WorkerCatalogLeases[2].Kind <> wckHistorical) then
  begin
    ErrorText := 'Scribe Setup refused an unclassified previous worker-pack catalog.';
    Exit;
  end;

  if WorkerCatalogLeases[0].Present then
  begin
    if WorkerCatalogLeases[0].Kind = wckHistorical then
    begin
      if WorkerCatalogLeases[2].Present then
      begin
        { In particular, A,B,A is never guessed at or mutated. }
        ErrorText := 'Scribe Setup refused an ambiguous historical worker-pack catalog recovery state.';
        Exit;
      end;
      if WorkerCatalogLeases[1].Present then
        WorkerCatalogMode := wcmRecoverUpdateBeforeFirstRename
      else
        WorkerCatalogMode := wcmUpdateStage;
    end
    else if WorkerCatalogLeases[1].Present then
      WorkerCatalogMode := wcmRepairRedundantNext
    else
      WorkerCatalogMode := wcmCommittedStage;
  end
  else if WorkerCatalogLeases[2].Present then
  begin
    if WorkerCatalogLeases[1].Present then
      WorkerCatalogMode := wcmRecoverUpdateBeforeSecondRename
    else
      WorkerCatalogMode := wcmMissingLiveRestage;
  end
  else if WorkerCatalogLeases[1].Present then
    WorkerCatalogMode := wcmRecoverFresh
  else
    WorkerCatalogMode := wcmFreshStage;

  WorkerCatalogStageRequired :=
    (WorkerCatalogMode = wcmFreshStage) or
    (WorkerCatalogMode = wcmUpdateStage) or
    (WorkerCatalogMode = wcmMissingLiveRestage) or
    (WorkerCatalogMode = wcmCommittedStage);

  if WorkerCatalogLeases[1].Present and
     not CurrentWorkerPackPayloadIsComplete(ErrorText) then
    Exit;
  Result := True;
end;

#ifdef WorkerCatalogPublicationTests
function WorkerCatalogTestFault(): String;
begin
  Result := ExpandConstant('{param:SCRIBECATALOGFAULT|}');
end;

function IsAllowedWorkerCatalogTestFault(const Value: String): Boolean;
begin
  Result :=
    (Value = '') or
    SameStr(Value, 'before-live-to-previous') or
    SameStr(Value, 'before-next-to-live') or
    SameStr(Value, 'occupy-previous') or
    SameStr(Value, 'occupy-live');
end;
#endif

function ShouldStageWorkerCatalog(): Boolean;
begin
  Result := WorkerCatalogStageRequired;
end;

procedure PrepareWorkerCatalogStaging();
begin
  ReleasePayloadHandleForCurrentFile();
end;

function FillWorkerCatalogRenameInformation(
  var RenameInformation: TFileRenameInformation;
  const TargetPath: String
): Boolean;
var
  Index: Integer;
begin
  Result := False;
  if (Length(TargetPath) = 0) or (Length(TargetPath) > 260) then
    Exit;
  RenameInformation.ReplaceIfExists := 0;
  { The pinned x86 Inno 6.7.1 ABI was proven with RootDirectory=NULL and a
    full absolute target. The root-handle variant fails with ERROR 87. }
  RenameInformation.RootDirectory := 0;
  RenameInformation.FileNameLength := Length(TargetPath) * 2;
  for Index := 0 to 259 do
    RenameInformation.FileName[Index] := #0;
  for Index := 1 to Length(TargetPath) do
    RenameInformation.FileName[Index - 1] := TargetPath[Index];
  Result := True;
end;

function MoveWorkerCatalogLease(
  SourceIndex: Integer;
  DestinationIndex: Integer;
  var ErrorText: String
): Boolean;
var
  RenameInformation: TFileRenameInformation;
  RetainedInformation: TByHandleFileInformation;
  RetainedFileSize: Int64;
  RetainedSha256: String;
  RetainedKind: TWorkerCatalogKind;
  ErrorCode: LongInt;
begin
  Result := False;
  if not WorkerCatalogLeases[SourceIndex].Present or
     WorkerCatalogLeases[DestinationIndex].Present then
  begin
    ErrorText := 'Scribe Setup refused an invalid catalog rename state.';
    Exit;
  end;
  if not RequireCatalogPathAbsent(CatalogPath(DestinationIndex), ErrorText) then
    Exit;
  if not UpgradeWorkerCatalogLeaseForMutation(SourceIndex, ErrorText) then
    Exit;
  RetainedInformation := WorkerCatalogLeases[SourceIndex].Information;
  RetainedFileSize := WorkerCatalogLeases[SourceIndex].FileSize;
  RetainedSha256 := WorkerCatalogLeases[SourceIndex].Sha256;
  RetainedKind := WorkerCatalogLeases[SourceIndex].Kind;
  if not FillWorkerCatalogRenameInformation(
    RenameInformation, CatalogPath(DestinationIndex)) then
  begin
    ErrorText := 'Scribe Setup refused an unsupported catalog rename target.';
    Exit;
  end;
  if not SetFileRenameInformationByHandle(
    WorkerCatalogLeases[SourceIndex].Handle,
    FileRenameInfo,
    RenameInformation,
    12 + RenameInformation.FileNameLength) then
  begin
    ErrorCode := DLLGetLastError;
    ErrorText := 'Scribe Setup could not publish a worker-pack catalog without replacing an occupied path (' +
      SysErrorMessage(ErrorCode) + ').';
    Exit;
  end;
  if not CloseHandle(WorkerCatalogLeases[SourceIndex].Handle) then
  begin
    ErrorCode := DLLGetLastError;
    WorkerCatalogLeases[SourceIndex].Handle := InvalidHandleValue;
    ErrorText := 'Scribe Setup could not close a renamed catalog handle (' +
      SysErrorMessage(ErrorCode) + ').';
    Exit;
  end;
  WorkerCatalogLeases[SourceIndex].Handle := InvalidHandleValue;
  WorkerCatalogLeases[SourceIndex].Path := CatalogPath(SourceIndex);
  WorkerCatalogLeases[SourceIndex].Present := False;
  WorkerCatalogLeases[SourceIndex].Kind := wckAbsent;
  WorkerCatalogLeases[SourceIndex].FileSize := -1;
  WorkerCatalogLeases[SourceIndex].Sha256 := '';
  if not RequireCatalogPathAbsent(CatalogPath(SourceIndex), ErrorText) then
    Exit;
  if not OpenWorkerCatalogLease(
    DestinationIndex, CatalogPath(DestinationIndex), ErrorText) then
    Exit;
  if not WorkerCatalogLeases[DestinationIndex].Present or
     not SameFileIdentity(
       RetainedInformation,
       WorkerCatalogLeases[DestinationIndex].Information) or
     (WorkerCatalogLeases[DestinationIndex].FileSize <> RetainedFileSize) or
     not SameStr(WorkerCatalogLeases[DestinationIndex].Sha256, RetainedSha256) or
     (WorkerCatalogLeases[DestinationIndex].Kind <> RetainedKind) then
  begin
    ErrorText := 'Scribe Setup refused a renamed catalog whose reopened identity or authenticated bytes changed.';
    Exit;
  end;
  Result := True;
end;

function DisposeRedundantNextCatalog(var ErrorText: String): Boolean;
var
  DispositionInformation: TFileDispositionInformation;
  ErrorCode: LongInt;
begin
  Result := False;
  if not WorkerCatalogLeases[1].Present or
     (WorkerCatalogLeases[1].Kind <> wckCurrent) or
     not ReopenAndMatchWorkerCatalogLease(1, ErrorText) then
    Exit;
  if not UpgradeWorkerCatalogLeaseForMutation(1, ErrorText) then
    Exit;
  DispositionInformation.DeleteFile := 1;
  if not SetFileDispositionInformationByHandle(
    WorkerCatalogLeases[1].Handle,
    FileDispositionInfo,
    DispositionInformation,
    SizeOf(DispositionInformation)) then
  begin
    ErrorCode := DLLGetLastError;
    ErrorText := 'Scribe Setup could not dispose the exact redundant staged catalog (' +
      SysErrorMessage(ErrorCode) + ').';
    Exit;
  end;
  CloseHandle(WorkerCatalogLeases[1].Handle);
  WorkerCatalogLeases[1].Handle := InvalidHandleValue;
  WorkerCatalogLeases[1].Present := False;
  WorkerCatalogLeases[1].Kind := wckAbsent;
  if not RequireCatalogPathAbsent(CatalogPath(1), ErrorText) then
    Exit;
  Result := True;
end;

function PreparePostInstallCatalogState(var ErrorText: String): Boolean;
var
  Index: Integer;
begin
  Result := False;
  for Index := 0 to 2 do
    if WorkerCatalogLeases[Index].Present then
    begin
      if not ReopenAndMatchWorkerCatalogLease(Index, ErrorText) then
        Exit;
    end
    else if (Index <> 1) or not WorkerCatalogStageRequired then
    begin
      if not RequireCatalogPathAbsent(CatalogPath(Index), ErrorText) then
        Exit;
    end;

  if WorkerCatalogStageRequired then
  begin
    if not OpenWorkerCatalogLease(1, CatalogPath(1), ErrorText) then
      Exit;
    if not WorkerCatalogLeases[1].Present or
       (WorkerCatalogLeases[1].Kind <> wckCurrent) then
    begin
      ErrorText := 'Scribe Setup did not stage the exact current worker-pack catalog.';
      Exit;
    end;
  end;
  Result := True;
end;

function VerifyPublishedLiveCatalog(var ErrorText: String): Boolean;
begin
  Result :=
    WorkerCatalogLeases[0].Present and
    (WorkerCatalogLeases[0].Kind = wckCurrent) and
    ReopenAndMatchWorkerCatalogLease(0, ErrorText);
  if not Result and (ErrorText = '') then
    ErrorText := 'Scribe Setup could not reverify the published live worker-pack catalog.';
end;

function CompleteWorkerCatalogPublication(var ErrorText: String): Boolean;
#ifdef WorkerCatalogPublicationTests
var
  CurrentFault: String;
#endif
begin
  Result := False;
  ErrorText := '';
  if WorkerCatalogMode = wcmUninitialized then
  begin
    ErrorText := 'Scribe Setup did not initialize worker-catalog publication.';
    Exit;
  end;
  CompactBoundHandles();
  if not PreparePostInstallCatalogState(ErrorText) then
    Exit;
  if not ValidateAndBindInstallTree(WorkerCatalogRoot, True, ErrorText) then
    Exit;
  if not CurrentWorkerPackPayloadIsComplete(ErrorText) then
    Exit;

#ifdef WorkerCatalogPublicationTests
  CurrentFault := WorkerCatalogTestFault();
#endif
  if (WorkerCatalogMode = wcmUpdateStage) or
     (WorkerCatalogMode = wcmRecoverUpdateBeforeFirstRename) then
  begin
#ifdef WorkerCatalogPublicationTests
    if SameStr(CurrentFault, 'before-live-to-previous') then
    begin
      ErrorText := 'Injected bounded failure before live-to-previous rename.';
      Exit;
    end;
    if SameStr(CurrentFault, 'occupy-previous') then
      if not SaveStringToFile(CatalogPath(2), 'occupied', False) then
      begin
        ErrorText := 'Could not create the bounded occupied-previous test fixture.';
        Exit;
      end;
#endif
    if not MoveWorkerCatalogLease(0, 2, ErrorText) then
      Exit;
#ifdef WorkerCatalogPublicationTests
    if SameStr(CurrentFault, 'before-next-to-live') then
    begin
      ErrorText := 'Injected bounded failure before next-to-live rename.';
      Exit;
    end;
    if SameStr(CurrentFault, 'occupy-live') then
      if not SaveStringToFile(CatalogPath(0), 'occupied', False) then
      begin
        ErrorText := 'Could not create the bounded occupied-live test fixture.';
        Exit;
      end;
#endif
    if not MoveWorkerCatalogLease(1, 0, ErrorText) then
      Exit;
  end
  else if (WorkerCatalogMode = wcmFreshStage) or
          (WorkerCatalogMode = wcmMissingLiveRestage) or
          (WorkerCatalogMode = wcmRecoverFresh) or
          (WorkerCatalogMode = wcmRecoverUpdateBeforeSecondRename) then
  begin
#ifdef WorkerCatalogPublicationTests
    if SameStr(CurrentFault, 'before-next-to-live') then
    begin
      ErrorText := 'Injected bounded failure before next-to-live rename.';
      Exit;
    end;
    if SameStr(CurrentFault, 'occupy-live') then
      if not SaveStringToFile(CatalogPath(0), 'occupied', False) then
      begin
        ErrorText := 'Could not create the bounded occupied-live test fixture.';
        Exit;
      end;
#endif
    if not MoveWorkerCatalogLease(1, 0, ErrorText) then
      Exit;
  end
  else if (WorkerCatalogMode = wcmCommittedStage) or
          (WorkerCatalogMode = wcmRepairRedundantNext) then
  begin
    if not DisposeRedundantNextCatalog(ErrorText) then
      Exit;
  end
  else
  begin
    ErrorText := 'Scribe Setup refused an unhandled worker-catalog publication state.';
    Exit;
  end;

  if not RequireCatalogPathAbsent(CatalogPath(1), ErrorText) or
     not VerifyPublishedLiveCatalog(ErrorText) then
    Exit;
  Result := True;
end;

procedure RecordWorkerCatalogLifecycleFailure(const ErrorText: String);
begin
  WorkerCatalogLifecycleSuccessful := False;
  WorkerCatalogLifecycleExitCode := WorkerCatalogFailureExitCode;
  Log('Worker catalog publication failed: ' + ErrorText);
  if not WizardSilent then
    MsgBox(
      ErrorText + #13#10 + #13#10 +
      'Scribe was not launched. Close any running Scribe process and run this installer again to recover the authenticated catalog state.',
      mbError,
      MB_OK);
end;

procedure InitializeWorkerCatalogLifecycle();
var
  Index: Integer;
begin
  WorkerCatalogRoot := '';
  WorkerCatalogMode := wcmUninitialized;
  WorkerCatalogStageRequired := False;
  WorkerCatalogLifecycleSuccessful := False;
  WorkerCatalogLifecycleExitCode := 0;
  ObservedCurrentWorkerPackFileCount := 0;
  for Index := 0 to 2 do
  begin
    WorkerCatalogLeases[Index].Handle := InvalidHandleValue;
    ClearWorkerCatalogLease(Index, False);
  end;
end;

function IsNormalInstallAndLifecycleSuccessful(): Boolean;
begin
  Result := IsNormalInstall() and WorkerCatalogLifecycleSuccessful;
end;

#ifdef WorkerCatalogPublicationTests
function WorkerCatalogTestLaunchRequested(): Boolean;
begin
  Result := ExpandConstant('{param:SCRIBECATALOGTESTLAUNCH|}') = '1';
end;

function ShouldRunCatalogTestLaunch(): Boolean;
begin
  Result :=
    WorkerCatalogLifecycleSuccessful and
    (ActiveTestToken() <> '') and
    WorkerCatalogTestLaunchRequested();
end;

function ResolveCatalogTestLaunchParameters(Param: String): String;
begin
  Result := '';
  if ShouldRunCatalogTestLaunch() then
    Result := '/D /C call "' + AddBackslash(TestShellRoot(ActiveTestToken())) +
      'run-sentinel.cmd"';
end;
#endif
