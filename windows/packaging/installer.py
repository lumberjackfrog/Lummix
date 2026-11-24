"""Build a current-user Windows Installer package from the exact qualified portable ZIP."""
from hashlib import sha256
from pathlib import Path
import sys
import tempfile
import uuid
import zipfile

import msilib
from msilib import schema, sequence

VERSION = "1.0.4"
FAMILY = uuid.UUID("10e85f80-e023-4eb2-9170-71537d744202")
def guid(text):
    return "{"+str(uuid.uuid5(FAMILY,text)).upper()+"}"

def ident(prefix,rel):
    return prefix+sha256(rel.encode("utf8")).hexdigest()[:28]

def main(archive, target):
    release=sha256(archive.read_bytes()).hexdigest()
    target.parent.mkdir(parents=True,exist_ok=True)
    product=guid("release/"+VERSION+"/"+release)
    db=msilib.init_database(str(target),schema,"Lummix",product,VERSION,"Lummix")
    info=db.GetSummaryInformation(4)
    info.SetProperty(msilib.PID_PAGECOUNT,500)
    # Compressed data and a limited-privilege install by the logged-on user.
    info.SetProperty(msilib.PID_WORDCOUNT,10)
    info.SetProperty(msilib.PID_APPNAME,"Lummix Windows package builder");info.Persist()
    def add(table,rows):msilib.add_data(db,table,rows)
    add("Property",[("UpgradeCode","{"+str(FAMILY).upper()+"}"),("SecureCustomProperties","LUMMIX_OLD;LUMMIX_NEW"),
                    ("ARPNOMODIFY","1"),("ARPPRODUCTICON","LummixIcon")])
    add("Upgrade",[("{"+str(FAMILY).upper()+"}",None,VERSION,None,512,None,"LUMMIX_OLD"),
                   ("{"+str(FAMILY).upper()+"}",VERSION,None,None,2,None,"LUMMIX_NEW")])
    add("LaunchCondition",[("Installed OR NOT LUMMIX_NEW","A newer Lummix is installed. Open it from Start."),
                           ("VersionNT64","Lummix requires 64-bit Windows.")])
    add("Directory",[("TARGETDIR",None,"SourceDir"),("LocalAppDataFolder","TARGETDIR","."),
                     ("Programs","LocalAppDataFolder","Programs"),("INSTALLDIR","Programs","Lummix"),
                     ("VERSIONDIR","INSTALLDIR",VERSION),("ProgramMenuFolder","TARGETDIR","."),
                     ("SHORTCUTDIR","ProgramMenuFolder","Lummix"),("DesktopFolder","TARGETDIR","."),
                     ("CacheBase","LocalAppDataFolder","Lummix"),
                     ("InstallerCache","CacheBase","Installer"),("ProductCache","InstallerCache",product)])
    add("Feature",[("Studio",None,"Lummix","Local desktop mixing studio",1,1,"INSTALLDIR",0)])
    with tempfile.TemporaryDirectory() as folder,zipfile.ZipFile(archive) as z:
        z.extractall(folder)
        root=Path(folder)/"Lummix"
        folders={".":"VERSIONDIR"}
        cab=msilib.CAB("studio.cab")
        exe_id=None
        for path in sorted(root.rglob("*")):
            rel=path.relative_to(root).as_posix()
            parent=folders[path.parent.relative_to(root).as_posix()]
            if path.is_dir():
                key=ident("d",rel);folders[rel]=key
                add("Directory",[(key,parent,path.name)])
                continue
            file_id=ident("f",rel);component=ident("c",rel)
            number,_=cab.append(str(path),path.name,file_id)
            add("Component",[(component,guid("files/"+VERSION+"/"+rel),parent,256,None,file_id)])
            add("FeatureComponents",[("Studio",component)])
            add("File",[(file_id,component,path.name,path.stat().st_size,None,None,512,number)])
            if rel=="Lummix.exe":exe_id=file_id
        assert exe_id
        cab.commit(db)
        msilib.add_data(db,"Icon",[("LummixIcon",msilib.Binary(str(root/"_internal/Lummix.ico")))])
    add("Component",[("ShortcutComponent",guid("startmenu"),"SHORTCUTDIR",260,None,"StartMenuKey")])
    add("FeatureComponents",[("Studio","ShortcutComponent")])
    add("Registry",[("StartMenuKey",1,r"Software\Lummix\Installer","StartMenu","[SHORTCUTDIR]","ShortcutComponent")])
    add("RegLocator",[("SavedMenu",1,r"Software\Lummix\Installer","StartMenu",18)])
    add("AppSearch",[("SHORTCUTDIR","SavedMenu")])
    add("Component",[("DesktopShortcut",guid("desktop-shortcut"),"DesktopFolder",260,None,"DesktopKey")])
    add("FeatureComponents",[("Studio","DesktopShortcut")])
    add("Registry",[("DesktopKey",1,r"Software\Lummix\Installer","DesktopShortcut","1","DesktopShortcut")])
    add("Shortcut",[("OpenStudio","SHORTCUTDIR","Lummix","ShortcutComponent","[#"+exe_id+"]",None,
                     "Open the Lummix studio",None,"LummixIcon",0,1,"VERSIONDIR"),
                    ("OpenStudioDesktop","DesktopFolder","Lummix","DesktopShortcut","[#"+exe_id+"]",None,
                     "Open the Lummix studio",None,"LummixIcon",0,1,"VERSIONDIR")])
    add("RemoveFile",[("RemoveMenu","ShortcutComponent",None,"SHORTCUTDIR",2),
                      ("RemoveCachedMSI","ShortcutComponent","Lummix.msi","ProductCache",2),
                      ("RemoveEmptyCache","ShortcutComponent",None,"ProductCache",2)])
    add("CustomAction",[("LaunchStudio",210,exe_id,None),
                        ("RemoveWristAddon",82,exe_id,"remove-addon")])
    actions = {"AppSearch","FindRelatedProducts","LaunchConditions","CostInitialize","FileCost","CostFinalize",
               "InstallValidate","InstallInitialize","ProcessComponents","UnpublishFeatures","RemoveShortcuts",
               "RemoveRegistryValues","RemoveFiles","RemoveFolders","CreateFolders","InstallFiles",
               "CreateShortcuts","WriteRegistryValues","RegisterUser","RegisterProduct","PublishFeatures",
               "PublishProduct","InstallFinalize"}
    add("InstallExecuteSequence", [(name,cond,n) for name,cond,n in sequence.InstallExecuteSequence if name in actions]+
        [("RemoveExistingProducts","LUMMIX_OLD",1501),
         ("RemoveWristAddon",'REMOVE="ALL" AND NOT UPGRADINGPRODUCTCODE',3100),
         ("LaunchStudio",'NOT Installed AND NOT REMOVE AND UILevel >= 3 AND NOT LUMMIX_NO_LAUNCH',6700)])
    add("InstallUISequence",[(name,cond,n) for name,cond,n in sequence.InstallUISequence if name in
                            {"AppSearch","FindRelatedProducts","LaunchConditions","CostInitialize","FileCost","CostFinalize","ExecuteAction"}])
    db.Commit()
    del info
    db.Close()
    print(target,product,target.stat().st_size,sha256(target.read_bytes()).hexdigest())
    return product

if __name__=="__main__":main(*(Path(value).resolve() for value in sys.argv[1:]))
