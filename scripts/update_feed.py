"""Validated public release mirror. GitHub is the authenticated provenance origin."""
from __future__ import annotations
from contextlib import contextmanager
import datetime as dt
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import tarfile
import tempfile
import urllib.error
import urllib.parse
import urllib.request
import zipfile
import zlib

REPOSITORY = 'ufna/zerus'
ORIGIN = 'https://zerus.dev'
PACKAGES = {'zerus','zerus-git','zerus-ade-bin','zerus-ade-nightly-bin'}
SAFE = re.compile(r'[A-Za-z0-9][A-Za-z0-9._+-]{0,179}\Z')
HEX40 = re.compile(r'[a-f0-9]{40}\Z')
HEX64 = re.compile(r'[a-f0-9]{64}\Z')
ASSET_LIMIT = 512*1024*1024
RELEASE_LIMIT = 2*1024*1024*1024
# Validation, archive and transport errors. Entry points report them with a fixed
# line only: their text can carry signed CDN URLs or credential context.
MIRROR_FAILURES = (ValueError, OSError, KeyError, TypeError, UnicodeError, EOFError, zlib.error,
                   zipfile.BadZipFile, tarfile.TarError, http.client.HTTPException)


def require(value, message):
    if not value: raise ValueError(message)


def safe_name(value):
    require(isinstance(value,str) and SAFE.fullmatch(value) and '..' not in value, 'Unsafe immutable release name')
    return value


def timestamp(value):
    require(isinstance(value,str) and len(value)<=40,'Invalid publication timestamp')
    try: parsed=dt.datetime.fromisoformat(value.replace('Z','+00:00'))
    except ValueError: raise ValueError('Invalid publication timestamp') from None
    require(parsed.tzinfo is not None,'Publication timestamp needs timezone')
    return parsed.astimezone(dt.timezone.utc)


def https(url, hosts):
    require(isinstance(url,str) and len(url)<=8192,'Invalid HTTPS address')
    try:
        value=urllib.parse.urlsplit(url); port=value.port
    except ValueError:raise ValueError('Invalid HTTPS address') from None
    require(value.scheme=='https' and value.hostname in hosts and port in (None,443)
            and value.username is None and value.password is None and not value.fragment,'Untrusted HTTPS origin')
    return url


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self,*args):return None


class GitHub:
    def asset_origin(self, url):
        return https(url, {'github.com', 'release-assets.githubusercontent.com', 'objects.githubusercontent.com'})

    def __init__(self,token=None):
        self.token=token
        self.opener=urllib.request.build_opener(NoRedirect)

    def stream(self,url,limit,*,api=False,accept='application/vnd.github+json',download=False):
        allowed={'api.github.com'} if api else {'github.com','release-assets.githubusercontent.com','objects.githubusercontent.com'}
        if api:https(url,allowed)
        else:self.asset_origin(url)
        for redirect in range(6):
            headers={'User-Agent':'zerus-update-mirror/1','Accept':accept}
            # Create every redirected request afresh: API credentials must never
            # reach GitHub's signed CDN URLs or release download endpoints.
            if api and urllib.parse.urlsplit(url).hostname=='api.github.com' and self.token:
                headers['Authorization']='Bearer '+self.token
            try: response=self.opener.open(urllib.request.Request(url,headers=headers),timeout=30)
            except urllib.error.HTTPError as error:
                code=error.code
                try:location=error.headers.get('Location')
                finally:error.close()
                if code not in (301,302,303,307,308):raise ValueError('GitHub request rejected (HTTP '+str(code)+')') from None
                require(location,'Missing download redirect')
                url=urllib.parse.urljoin(url,location)
                # No authentication survives an asset redirect, even if it points
                # back to the API. Actions archive requests use the JSON media
                # type but explicitly allow downloads; metadata never redirects.
                require(download or accept=='application/octet-stream','Unexpected API redirect')
                api=False;self.asset_origin(url);accept='application/octet-stream'
                continue
            except (OSError,urllib.error.URLError,http.client.HTTPException):raise ValueError('GitHub transport unavailable') from None
            with response:
                length=response.headers.get('Content-Length')
                require(length is None or length.isdecimal() and int(length)<=limit,'Upstream body exceeds limit')
                total=0
                while True:
                    try:block=response.read(65536)
                    except (OSError,http.client.HTTPException):raise ValueError('GitHub transport interrupted') from None
                    if not block:break
                    total+=len(block);require(total<=limit,'Upstream body exceeds limit')
                    yield block
            return
        raise ValueError('Too many download redirects')

    def api(self,path):
        require(path.startswith('/repos/'+REPOSITORY+'/'),'Unexpected repository API path')
        try:return json.loads(b''.join(self.stream('https://api.github.com'+path,4*1024*1024,api=True)))
        except (ValueError,UnicodeError) as error:
            if isinstance(error,UnicodeError):raise ValueError('Invalid GitHub JSON') from None
            raise

    def releases(self):
        rows=[]
        for page in range(1,11):
            batch=self.api('/repos/'+REPOSITORY+f'/releases?per_page=100&page={page}')
            require(isinstance(batch,list),'Invalid release catalog')
            rows.extend(batch)
            if len(batch)<100:return rows
        raise ValueError('Release catalog exceeds bounded pagination')

    def tag_commit(self,tag):
        safe_name(tag)
        value=self.api('/repos/'+REPOSITORY+'/git/ref/tags/'+tag)['object']
        for _ in range(5):
            require(isinstance(value,dict) and HEX40.fullmatch(value.get('sha','')),'Invalid tag provenance')
            if value.get('type')=='commit':return value['sha']
            require(value.get('type')=='tag','Tag must resolve to a commit')
            value=self.api('/repos/'+REPOSITORY+'/git/tags/'+value['sha'])['object']
        raise ValueError('Tag indirection exceeds bounds')

    def asset(self,asset,destination):
        require(type(asset.get('id')) is int and asset['id']>0,'Invalid GitHub asset identity')
        limit=asset['size']
        digest=hashlib.sha256();total=0
        with destination.open('xb') as output:
            for block in self.stream('https://api.github.com/repos/'+REPOSITORY+'/releases/assets/'+str(asset['id']),limit,api=True,accept='application/octet-stream'):
                output.write(block);digest.update(block);total+=len(block)
        require(total==limit,'GitHub asset length mismatch')
        declared=asset.get('digest')
        require(declared is None or declared=='sha256:'+digest.hexdigest(),'GitHub asset digest mismatch')
        return digest.hexdigest()


def release_assets(release):
    tag=safe_name(release.get('tag_name'));require(type(release.get('id')) is int and release['id']>0,'Invalid release identity')
    require(release.get('draft') is False and type(release.get('prerelease')) is bool,'Unpublished release')
    timestamp(release.get('published_at'))
    require(release.get('html_url')=='https://github.com/'+REPOSITORY+'/releases/tag/'+tag,'Wrong repository release URL')
    assets=release.get('assets');require(isinstance(assets,list) and 1<=len(assets)<=64,'Invalid release assets')
    rows={};total=0
    for asset in assets:
        name=safe_name(asset.get('name'));require(name not in rows,'Duplicate release filename')
        require(type(asset.get('size')) is int and 0<asset['size']<=ASSET_LIMIT,'Invalid asset size')
        require(asset.get('state')=='uploaded','Release asset upload incomplete')
        require(asset.get('browser_download_url')=='https://github.com/'+REPOSITORY+'/releases/download/'+tag+'/'+name,'Asset repository/tag mismatch')
        total+=asset['size'];rows[name]=asset
    require(total<=RELEASE_LIMIT,'Release exceeds mirror budget')
    require({'release-info.json','SHA256SUMS'}<=rows.keys(),'Release lacks sealed metadata')
    return rows


def windows_portable(version):
    return f'zerus-{version}-windows-x86_64.zip'


def checksums(raw):
    require(len(raw)<=256*1024,'Checksum list exceeds bounds')
    result={}
    for line in raw.decode('ascii').splitlines():
        match=re.fullmatch(r'([a-f0-9]{64})  ([A-Za-z0-9][A-Za-z0-9._+-]{0,179})',line)
        require(match is not None,'Invalid checksum line')
        digest,name=match.groups();safe_name(name)
        require(name not in result and name!='SHA256SUMS','Duplicate/self-referential checksum')
        result[name]=digest
    require(bool(result),'Empty checksum list')
    return result


def aur_versions(path):
    """Read bounded .SRCINFO data; never extract or execute package recipes."""
    versions={};seen=set();size=0
    try:
        with tarfile.open(path,'r:gz') as archive:
            for index,member in enumerate(archive):
                require(index<32 and member.isfile() and member.size<=256*1024,'Unsafe AUR metadata member')
                match=re.fullmatch(r'aur/(zerus(?:-git|-ade-bin|-ade-nightly-bin)?)/(PKGBUILD|\.SRCINFO)',member.name)
                require(match and member.name not in seen,'Unexpected/duplicate AUR metadata path')
                seen.add(member.name);size+=member.size;require(size<=2*1024*1024,'AUR metadata exceeds bounds')
                if match[2]!='.SRCINFO':continue
                content=archive.extractfile(member).read().decode('utf-8');fields={}
                for line in content.splitlines():
                    key,sep,value=line.strip().partition(' = ')
                    if sep and key in {'pkgbase','pkgname','pkgver','pkgrel','epoch'}:
                        require(key not in fields,'Duplicate package identity field');fields[key]=value
                require(fields.get('pkgbase')==match[1] and fields.get('pkgname')==match[1],'AUR package identity mismatch')
                version=fields.get('pkgver','');rel=fields.get('pkgrel','');epoch=fields.get('epoch')
                require(re.fullmatch(r'[0-9][A-Za-z0-9.+_~:-]{0,119}',version) and re.fullmatch(r'[0-9]+(?:\.[0-9]+)*',rel),'Invalid actual package version')
                require(epoch is None or re.fullmatch(r'[0-9]+',epoch),'Invalid package epoch')
                versions[match[1]]=(epoch+':' if epoch else '')+version+'-'+rel
    except (tarfile.TarError,EOFError,zlib.error):
        # A truncated or corrupt sealed archive fails like other invalid metadata.
        raise ValueError('Invalid AUR metadata archive') from None
    require(bool(versions),'Missing actual package versions')
    return versions


def build_feed(release,info,files,sums,package_versions,commit):
    tag=safe_name(release['tag_name']);require(info.get('schema') in (1,2) and type(info.get('schema')) is int and (info['schema']==1)==('macos' not in info),'Unsupported release metadata schema')
    require(info.get('source_commit')==commit and HEX40.fullmatch(commit),'Release tag/source commit mismatch')
    platform=info.get('platform','desktop');channel=info.get('channel','stable')
    require(platform in {'android','desktop'} and channel in {'dev','stable','nightly'},'Unknown release platform/channel')
    require(type(info.get('source_dirty',False)) is bool,'Invalid source snapshot provenance')
    dirty=info.get('source_dirty',False);require(not dirty or platform=='android' and channel=='dev','Dirty sources are development Android snapshots only')
    version=info.get('version_name') if platform=='android' else info.get('version')
    require(isinstance(version,str) and 1<=len(version)<=(80 if platform=='android' else 120) and re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9.+_-]*',version),'Invalid product version')
    if platform=='desktop':
        require(info.get('architecture')=='x86_64' and info.get('distribution')=='Arch Linux' and info.get('native_agents_bundled') is False,'Unexpected desktop platform/runtime')
        if channel=='stable':require(re.fullmatch(r'\d+\.\d+\.\d+',version) and tag=='v'+version and release['prerelease'] is False,'Stable tag/version mismatch')
        elif channel=='nightly':
            require(release['prerelease'] is True and type(info.get('workflow_run_id')) is int and tag=='nightly-'+str(info['workflow_run_id']) and info.get('release_tag')==tag,'Nightly provenance mismatch')
            product=info.get('product_version');count=info.get('revision_count');number=info.get('workflow_run_number')
            require(isinstance(product,str) and re.fullmatch(r'\d+\.\d+\.\d+',product) and type(count) is int and count>0 and type(number) is int and number>0,'Invalid nightly build provenance')
            require(version==f'{product}.r{count}.g{commit[:7]}.n{number}','Nightly source version mismatch')
        else:raise ValueError('Desktop dev publication is not configured')
        family={'zerus','zerus-ade-bin','zerus-git'} if channel=='stable' else {'zerus-ade-nightly-bin'}
        require(set(info.get('aur_packages',[]))==set(package_versions)==family,'AUR family/projection mismatch')
        expected={'release-info.json',f'zerus-{version}-aur.tar.gz',f'zerus-{version}-arch-x86_64.tar.gz'}
        for package in family-{'zerus-git'}:
            actual=package_versions[package].split(':')[-1]
            require(actual.rsplit('-',1)[0]==version,'Package/product version mismatch')
            expected.add(f'{package}-{actual}-x86_64.pkg.tar.zst')
        if channel=='stable':expected.add(f'zerus-{version}-source.tar.gz')
        portable=info.get('windows_portable')
        if portable is not None:
            # Stable and nightly releases may carry it. Linked from GitHub only;
            # deployed GUIs reject unknown feed artifact kinds.
            require(portable==windows_portable(version),'Unexpected Windows portable asset');expected.add(portable)
        installer=info.get('windows_installer')
        if installer is not None:
            require(channel in {'stable','nightly'} and isinstance(installer,dict) and
                    set(installer)=={'asset','portable_sha256'} and
                    installer['asset']==f'zerus-{version}-windows-x86_64-setup.exe' and
                    portable==windows_portable(version) and
                    isinstance(installer['portable_sha256'],str) and
                    re.fullmatch(r'[0-9a-f]{64}',installer['portable_sha256']) and
                    sums.get(portable)==installer['portable_sha256'],
                    'Unexpected Windows installer asset or paired ZIP digest')
            expected.add(installer['asset'])
        macos=info.get('macos')
        if macos is not None:
            # Schema 2 adds the notarized Apple Silicon bundle of the channel's Homebrew cask.
            # Homebrew updates it, so the catalog keeps listing only artifacts existing clients know.
            cask={'stable':'zerus','nightly':'zerus@nightly'}.get(channel)
            require(cask is not None and info.get('schema')==2 and info.get('homebrew_casks')==[cask] and isinstance(macos,dict) and macos.get('asset')==f'zerus-{version}-macos-arm64.zip','Unexpected macOS release metadata')
            expected.add(macos['asset'])
            if 'dmg' in macos:
                dmg=macos['dmg']
                require(isinstance(dmg,dict) and set(dmg)=={'asset','notarization_id'} and
                        dmg['asset']==f'zerus-{version}-macos-arm64.dmg' and isinstance(dmg['notarization_id'],str) and
                        re.fullmatch(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}',dmg['notarization_id']),
                        'Unexpected macOS disk image metadata')
                expected.add(dmg['asset'])
        require(set(sums)==expected,'Unexpected desktop asset family')
    artifacts=[]
    declared=info.get('artifacts') if platform=='android' else None
    if platform=='android':
        require(tag.startswith(('android-','mobile-')) and isinstance(declared,list) and 1<=len(declared)<=4,'Missing Android artifact metadata')
        require(type(info.get('version_code')) is int and 0<info['version_code']<=2_100_000_000,'Invalid Android version code')
        require(info.get('package_name')=='app.zerus.mobile' and type(info.get('min_sdk')) is int and 26<=info['min_sdk']<=100 and HEX64.fullmatch(info.get('signing_cert_sha256','')),'Missing verified APK identity')
        require(release['prerelease'] is (channel!='stable'),'Android channel/prerelease mismatch')
        names=[safe_name(row.get('name')) for row in declared]
        require(len(names)==len(set(names)) and set(sums)==set(names)|{'release-info.json'},'Unlisted/duplicate Android assets')
        for row in declared:
            name=safe_name(row.get('name'));require(name in sums and name in files and row.get('sha256')==sums[name] and row.get('size')==files[name].stat().st_size,'Android artifact seal mismatch')
            require(row.get('kind') in {'apk','source'} and name.endswith('.apk' if row['kind']=='apk' else '.tar.gz'),'Unexpected Android artifact kind/name')
            entry={'kind':row['kind'],'name':name,'url':ORIGIN+'/downloads/'+tag+'/'+name,'sha256':sums[name],'size':row['size'],'os':'android' if row['kind']=='apk' else 'any','arch':'universal' if row['kind']=='apk' else 'source'}
            if row['kind']=='apk':entry.update(minSdk=info['min_sdk'],packageName=info['package_name'],signingCertSha256=info['signing_cert_sha256'])
            artifacts.append(entry)
        require(sum(row['kind']=='apk' for row in artifacts)==1,'Expected one verified APK')
    else:
        for name in sorted(sums):
            if name.endswith('-source.tar.gz'):kind,arch='source','source'
            elif name.endswith('-arch-x86_64.tar.gz'):kind,arch='linux-archive','x86_64'
            elif name.endswith('.pkg.tar.zst'):kind,arch='arch-package','x86_64'
            else:continue
            artifacts.append({'kind':kind,'name':name,'url':ORIGIN+'/downloads/'+tag+'/'+name,'sha256':sums[name],'size':files[name].stat().st_size,'os':'any' if kind=='source' else 'linux','arch':arch})
        require(any(row['kind']=='arch-package' for row in artifacts),'Desktop release lacks actual checked package')
    feed={'schemaVersion':1,'product':'zerus','platform':platform,'channel':channel,'versionName':version,'releaseId':release['id'],'publishedAt':release['published_at'],'releaseUrl':release['html_url'],'sourceCommit':commit,'artifacts':artifacts}
    if platform=='desktop':
        feed['packages']=package_versions
        if info.get('macos') is not None:
            from macos_release import catalog_macos, macos_assets
            assets=release_assets(release)
            seals={name:{'size':assets[name]['size'],'sha256':sums[name]} for name in macos_assets(info['macos'])}
            feed['macos']=catalog_macos(info['macos'],version,channel,
                                        f'https://github.com/{REPOSITORY}/releases/download/{tag}/',seals)
    else:feed['versionCode']=info['version_code']
    if dirty:
        snapshot=info.get('source_snapshot');require(isinstance(snapshot,dict),'Development snapshot lacks source archive')
        selected=[a for a in artifacts if a['kind']=='source' and a['name']==snapshot.get('name') and a['sha256']==snapshot.get('sha256') and a['size']==snapshot.get('size')]
        require(len(selected)==1,'Development source snapshot seal mismatch');feed.update(sourceDirty=True,sourceSnapshot=selected[0])
    return feed


class ChannelMismatch(ValueError):
    """A valid Android release may belong to a different mobile channel."""


def file_hash(path):
    require(path.is_file() and not path.is_symlink(),'Nonregular mirror asset')
    digest=hashlib.sha256()
    with path.open('rb') as content:
        while block:=content.read(65536):digest.update(block)
    return digest.hexdigest()


def validate_and_fetch(client,release,directory,expected_platform,expected_channel,cache_root=None):
    assets=release_assets(release)
    require(assets['release-info.json']['size']<=512*1024 and assets['SHA256SUMS']['size']<=256*1024,'Release metadata exceeds limits')
    cached=cache_root/'downloads'/release['tag_name'] if cache_root else None
    require(cached is None or not cached.is_symlink(),'Symlinked immutable release')
    files={}
    def fetch(name,expected=None):
        asset=assets[name];old=cached/name if cached else None
        declared=asset.get('digest')
        require(declared is None or isinstance(declared,str) and re.fullmatch(r'sha256:[a-f0-9]{64}',declared),'Invalid GitHub digest')
        # Metadata without a GitHub digest is fetched anew; payload bytes can be
        # reused against the freshly verified SHA256SUMS. Never trust size alone.
        if old and old.exists() and (expected or declared):
            require(old.stat().st_size==asset['size'],'Immutable asset size collision')
            actual=file_hash(old)
            require(not expected or actual==expected,'Immutable asset checksum collision')
            require(not declared or declared=='sha256:'+actual,'Immutable GitHub digest collision')
            files[name]=old;return old
        path=directory/name;actual=client.asset(asset,path)
        require(not expected or actual==expected,'Checksum verification failed')
        files[name]=path;return path
    sums=checksums(fetch('SHA256SUMS').read_bytes())
    require(set(sums)==set(assets)-{'SHA256SUMS'},'Unsealed/unexpected release assets')
    try:info=json.loads(fetch('release-info.json',sums['release-info.json']).read_text())
    except (ValueError,UnicodeError):raise ValueError('Invalid sealed release metadata') from None
    require(isinstance(info,dict),'Invalid sealed release metadata')
    if info.get('platform','desktop')!=expected_platform or info.get('channel','stable')!=expected_channel:
        raise ChannelMismatch('Selected release belongs to another channel')
    portable=info.get('windows_portable') if expected_platform=='desktop' and expected_channel in {'stable','nightly'} else None
    installer=info.get('windows_installer') if expected_platform=='desktop' else None
    setup=installer.get('asset') if isinstance(installer,dict) else None
    macos=info.get('macos') if expected_platform=='desktop' else None
    cask=macos.get('asset') if isinstance(macos,dict) else None
    dmg=macos.get('dmg') if isinstance(macos,dict) else None
    image=dmg.get('asset') if isinstance(dmg,dict) else None
    for name,digest in sums.items():
        if name in files:continue
        if (isinstance(portable,str) and name==portable and name.endswith('-windows-x86_64.zip')) or \
                (isinstance(setup,str) and name==setup and name.endswith('-windows-x86_64-setup.exe')) or \
                (isinstance(cask,str) and name==cask and name.endswith('-macos-arm64.zip')) or \
                (isinstance(image,str) and name==image and name.endswith('-macos-arm64.dmg')):
            # Not mirrored (the mirror never deletes and has a fixed disk budget)
            # and not a feed artifact; build_feed checks its sealed name. Homebrew
            # downloads the macOS zip from GitHub; direct installs use the DMG.
            # Windows installers also stay on GitHub, alongside their paired ZIP.
            declared=assets[name].get('digest')
            require(declared is None or declared=='sha256:'+digest,'GitHub asset digest mismatch')
            continue
        fetch(name,digest)
    packages={}
    if expected_platform=='desktop':
        aur=[path for name,path in files.items() if name.endswith('-aur.tar.gz')];require(len(aur)==1,'Expected sealed AUR metadata archive');packages=aur_versions(aur[0])
    commit=client.tag_commit(release['tag_name'])
    return build_feed(release,info,files,sums,packages,commit),files


def candidates(rows,platform,channel):
    eligible=[]
    for row in rows:
        tag=row.get('tag_name','')
        if row.get('draft') is not False:continue
        if platform=='desktop' and channel=='stable':match=re.fullmatch(r'v\d+\.\d+\.\d+',tag) and row.get('prerelease') is False
        elif platform=='desktop' and channel=='nightly':match=re.fullmatch(r'nightly-\d+',tag) and row.get('prerelease') is True
        elif platform=='android':match=tag.startswith(('android-','mobile-'))
        else:match=False
        if match:eligible.append(row)
    eligible.sort(key=lambda row:(timestamp(row['published_at']),row['id']),reverse=True)
    if platform=='desktop' and channel=='stable':eligible.sort(key=lambda row:tuple(map(int,row['tag_name'][1:].split('.'))),reverse=True)
    return eligible[:20]


def private_root(root):
    root=Path(root)
    require(root.is_absolute() and not root.is_symlink(),'Mirror root must be a dedicated absolute directory')
    root.mkdir(parents=True,exist_ok=True);require(root.is_dir(),'Mirror root is not a directory')
    return root


def directory(root,*parts):
    target=root
    for part in parts:
        safe_name(part);target=target/part
        require(not target.is_symlink(),'Symlinked mirror directory')
        target.mkdir(exist_ok=True,mode=0o755);require(target.is_dir(),'Invalid mirror directory')
    return target


def check_previous(path,feed):
    require(not path.is_symlink(),'Symlinked feed')
    if not path.exists():return
    require(path.stat().st_size<=256*1024,'Existing feed exceeds bounds')
    old=json.loads(path.read_text())
    require(old['platform']==feed['platform'] and old['channel']==feed['channel'],'Existing feed scope mismatch')
    if old['releaseId']==feed['releaseId']:
        # Older catalogs omitted the sealed Mac downloads. Permit only that
        # additive projection after validate_and_fetch verifies the same release.
        additive = 'macos' not in old and 'macos' in feed and feed['platform']=='desktop'
        require(old==({k:v for k,v in feed.items() if k!='macos'} if additive else feed),
                'Immutable release metadata changed')
    else:
        require(timestamp(feed['publishedAt'])>=timestamp(old['publishedAt']),'Refusing feed publication rollback')
        if feed['platform']=='android':require(feed['versionCode']>old['versionCode'],'Refusing APK downgrade/same-code replacement')
        elif feed['channel']=='stable':
            require(tuple(map(int,feed['versionName'].split('.')))>=tuple(map(int,old['versionName'].split('.'))),'Refusing desktop downgrade')


def fsync_directory(path):
    handle=os.open(path,os.O_DIRECTORY)
    try:os.fsync(handle)
    finally:os.close(handle)


def check_android_sequence(root, feed, current_path):
    """Observe all Android channels under the publisher's shared mirror lock."""
    if feed.get('platform') != 'android':
        return
    require(not root.is_symlink() and (not root.exists() or root.is_dir()), 'Invalid Android mirror root')
    code = feed.get('versionCode')
    require(type(code) is int and 0 < code <= 2_100_000_000, 'Invalid Android publication code')
    highest = 0
    unchanged = False
    for schema, channel in ((1, 'stable'), (1, 'dev'), (1, 'nightly'), (2, 'dev')):
        path = root
        parts = ('updates', f'v{schema}', 'android', channel + '.json')
        for index, part in enumerate(parts):
            path = path / part
            require(not path.is_symlink(), 'Symlinked Android publication catalog')
            if path.exists():
                require(path.is_file() if index == len(parts)-1 else path.is_dir(),
                        'Invalid Android publication catalog path')
        if not path.exists():
            continue
        require(path.stat().st_size <= 256*1024, 'Android publication catalog exceeds bounds')
        old = json.loads(path.read_text())
        require(isinstance(old, dict) and old.get('schemaVersion') == schema
                and type(old.get('schemaVersion')) is int and old.get('platform') == 'android'
                and old.get('channel') == channel, 'Android publication catalog scope mismatch')
        previous = old.get('versionCode')
        require(type(previous) is int and 0 < previous <= 2_100_000_000,
                'Invalid published Android code')
        highest = max(highest, previous)
        if path == current_path and old == feed:
            unchanged = True
    # Full seal/byte verification precedes this check. Retaining the exact
    # current catalog is not a new APK publication after another channel advances.
    require(unchanged or code > highest, 'Android code must exceed every published channel')


@contextmanager
def mirror_lock(root, execute):
    """Serialize publication and inspect existing catalogs under the same lock."""
    import fcntl  # Only the Unix mirror needs locking; APK sealing is portable.
    require(root.is_absolute() and not root.is_symlink(), 'Dedicated absolute mirror root required')
    if not execute and not root.exists():
        # An absent mirror has no published floor. A dry run creates no root.
        yield root
        return
    root = private_root(root) if execute else root
    require(root.is_dir(), 'Invalid mirror root')
    lock = root / '.mirror.lock'
    require(not lock.is_symlink(), 'Symlinked mirror lock')
    with lock.open('a') as handle:
        fcntl.flock(handle, (fcntl.LOCK_EX if execute else fcntl.LOCK_SH) | fcntl.LOCK_NB)
        yield root


def publish(root,feed,files):
    path=directory(root,'updates','v1',feed['platform'])/(feed['channel']+'.json')
    check_previous(path,feed)
    check_android_sequence(root,feed,path)
    target=directory(root,'downloads',safe_name(feed['releaseUrl'].rsplit('/',1)[1]))
    for name,source in files.items():
        destination=target/safe_name(name)
        require(not destination.is_symlink(),'Symlinked immutable asset')
        if destination.exists():
            require(destination.is_file() and destination.stat().st_size==source.stat().st_size and file_hash(destination)==file_hash(source),'Immutable asset collision');continue
        temporary=None
        try:
            with tempfile.NamedTemporaryFile(dir=target,delete=False) as output:
                temporary=Path(output.name)
                with source.open('rb') as content:
                    while block:=content.read(65536):output.write(block)
                output.flush();os.fsync(output.fileno())
            temporary.chmod(0o644);os.link(temporary,destination)
        finally:
            if temporary:temporary.unlink(missing_ok=True)
    fsync_directory(target)
    data=(json.dumps(feed,indent=2,sort_keys=True)+'\n').encode();require(len(data)<=256*1024,'Feed exceeds size limit')
    if path.exists() and path.read_bytes()==data:return
    temporary=None
    try:
        with tempfile.NamedTemporaryFile(dir=path.parent,delete=False) as output:
            temporary=Path(output.name);output.write(data);output.flush();os.fsync(output.fileno())
        temporary.chmod(0o644);os.replace(temporary,path)
        fsync_directory(path.parent)
    finally:
        if temporary:temporary.unlink(missing_ok=True)
