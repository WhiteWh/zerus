"""Isolated release mirror contract: no GitHub writes or product state."""
import contextlib
import copy
import gzip
import hashlib
import http.client
import importlib.util
import io
import json
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
import urllib.error
import zlib
from email.message import Message
from unittest.mock import patch

SCRIPTS=Path(__file__).resolve().parents[1]/'scripts'
sys.path.insert(0,str(SCRIPTS))
import update_feed as feed
spec=importlib.util.spec_from_file_location('publisher',SCRIPTS/'publish-update-feed.py')
publisher=importlib.util.module_from_spec(spec);spec.loader.exec_module(publisher)

COMMIT='a'*40
FAILED='Mirror validation or transport failed; previous catalogs retained\n'


def archive(packages):
    raw=io.BytesIO()
    with tarfile.open(fileobj=raw,mode='w:gz') as output:
        for name,version in packages.items():
            pkgver,pkgrel=version.rsplit('-',1)
            data=f'pkgbase = {name}\n\tpkgver = {pkgver}\n\tpkgrel = {pkgrel}\npkgname = {name}\n'.encode()
            member=tarfile.TarInfo(f'aur/{name}/.SRCINFO');member.size=len(data);output.addfile(member,io.BytesIO(data))
    return raw.getvalue()


class FakeGitHub:
    def __init__(self,android=False,nightly=False,windows=False,stable='0.37.0',installer=False):
        self.calls=[];self.commit=COMMIT;self.rows=[];self.blobs={}
        if android:
            tag='android-dev-1';version='0.1.7';package=b'checked apk fixture';source=b'checked source fixture'
            self.info={'schema':1,'platform':'android','channel':'dev','version_name':version,'version_code':8,'package_name':'app.zerus.mobile','min_sdk':26,'signing_cert_sha256':'b'*64,'source_commit':COMMIT,'source_dirty':True,
                       'artifacts':[{'kind':'apk','name':'zerus.apk','sha256':hashlib.sha256(package).hexdigest(),'size':len(package)}, {'kind':'source','name':'zerus-source.tar.gz','sha256':hashlib.sha256(source).hexdigest(),'size':len(source)}],
                       'source_snapshot':{'name':'zerus-source.tar.gz','sha256':hashlib.sha256(source).hexdigest(),'size':len(source)}}
            payload={'zerus.apk':package,'zerus-source.tar.gz':source}
        else:
            version='0.37.0.r123.gaaaaaaa.n5' if nightly else stable;tag='nightly-1234' if nightly else 'v'+version
            packages={'zerus-ade-nightly-bin':version+'-1'} if nightly else {'zerus':version+'-1','zerus-ade-bin':version+'-1','zerus-git':'0.37.0.r91.g1234567-2'}
            self.info={'schema':1,'version':version,'source_commit':COMMIT,'architecture':'x86_64','distribution':'Arch Linux','native_agents_bundled':False,'aur_packages':list(packages)}
            if nightly:self.info.update(channel='nightly',product_version='0.37.0',revision_count=123,workflow_run_id=1234,workflow_run_number=5,release_tag=tag)
            payload={f'zerus-{version}-aur.tar.gz':archive(packages),f'zerus-{version}-arch-x86_64.tar.gz':b'binary tar'}
            if windows:
                self.info['windows_portable']=f'zerus-{version}-windows-x86_64.zip';payload[self.info['windows_portable']]=b'portable zip'
            if installer:
                setup=f'zerus-{version}-windows-x86_64-setup.exe'
                self.info['windows_installer']={'asset':setup,'portable_sha256':hashlib.sha256(payload[self.info['windows_portable']]).hexdigest()}
                payload[setup]=b'MZinstaller'
            if not nightly:payload[f'zerus-{version}-source.tar.gz']=b'source tar'
            for name,actual in packages.items():
                if name!='zerus-git':payload[f'{name}-{actual}-x86_64.pkg.tar.zst']=b'package '+name.encode()
        self.release={'id':10,'draft':False,'prerelease':android or nightly,'tag_name':tag,'published_at':'2026-10-09T12:00:00Z','html_url':f'https://github.com/ufna/zerus/releases/tag/{tag}'}
        self.payload=payload;self.seal()

    def seal(self):
        payload={**self.payload,'release-info.json':json.dumps(self.info).encode()}
        payload['SHA256SUMS']=''.join(hashlib.sha256(raw).hexdigest()+'  '+name+'\n' for name,raw in sorted(payload.items())).encode()
        self.release['assets']=[];self.blobs={}
        for index,(name,raw) in enumerate(payload.items(),1):
            self.blobs[index]=raw
            self.release['assets'].append({'id':index,'name':name,'size':len(raw),'digest':'sha256:'+hashlib.sha256(raw).hexdigest(),'state':'uploaded','browser_download_url':f'https://github.com/ufna/zerus/releases/download/{self.release["tag_name"]}/{name}'})
        self.rows=[self.release]

    def releases(self):return self.rows
    def tag_commit(self,tag):return self.commit
    def asset(self,row,path):
        self.calls.append(row['name']);raw=self.blobs[row['id']];path.write_bytes(raw)
        digest=hashlib.sha256(raw).hexdigest()
        feed.require(len(raw)==row['size'] and row.get('digest')=='sha256:'+digest,'Fake upstream byte mismatch')
        return digest


class MirrorTest(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name)/'content';self.client=FakeGitHub()
    def tearDown(self):self.temp.cleanup()
    def run_mirror(self,execute=True,selector='desktop/stable'):
        return publisher.synchronize(self.client,self.root,[selector],execute)
    def test_dry_run_writes_nothing_and_projects_actual_vcs_package(self):
        self.run_mirror(False);self.assertFalse(self.root.exists())
        with tempfile.TemporaryDirectory() as stage:
            result,_=feed.validate_and_fetch(self.client,self.client.release,Path(stage),'desktop','stable')
        self.assertEqual(result['packages']['zerus-git'],'0.37.0.r91.g1234567-2')
    def test_immutable_noop_redownloads_no_assets_and_retains_pointer(self):
        self.root.mkdir();self.run_mirror();path=self.root/'updates/v1/desktop/stable.json';before=path.stat().st_mtime_ns
        self.client.calls.clear();self.run_mirror();self.assertEqual(self.client.calls,[]);self.assertEqual(path.stat().st_mtime_ns,before)
        self.assertTrue((self.root/'downloads/v0.37.0/SHA256SUMS').is_file())
    def add_macos(self,asset='zerus-0.37.0-macos-arm64.zip',declared=True,cask='zerus'):
        if declared:self.client.info.update(schema=2,homebrew_casks=[cask],macos={'asset':asset,'qt_version':'6.11.2','team_id':'ABCDE12345','notarization_id':'0123abcd-4567-89ef-0123-456789abcdef','minimum_system_version':'14.0'})
        self.client.payload[asset]=b'notarized application';self.client.seal()
    def test_stable_macos_cask_asset_stays_on_github_and_out_of_the_catalog(self):
        self.add_macos();self.root.mkdir();self.run_mirror()
        data=json.loads((self.root/'updates/v1/desktop/stable.json').read_text())
        self.assertNotIn('zerus-0.37.0-macos-arm64.zip',[row['name'] for row in data['artifacts']])
        self.assertEqual({row['kind'] for row in data['artifacts']},{'source','linux-archive','arch-package'})
        # Homebrew downloads it from GitHub; the mirror never deletes and has a fixed disk budget.
        self.assertFalse((self.root/'downloads/v0.37.0/zerus-0.37.0-macos-arm64.zip').exists())
        self.assertNotIn('zerus-0.37.0-macos-arm64.zip',self.client.calls)
        download=data['macos']['downloads'][0]
        self.assertEqual(download['url'],'https://github.com/ufna/zerus/releases/download/v0.37.0/'+download['name'])
        self.assertEqual(download['sha256'],hashlib.sha256(self.client.payload[download['name']]).hexdigest())
    def test_nightly_macos_cask_asset_stays_on_github_and_out_of_the_catalog(self):
        asset='zerus-0.37.0.r123.gaaaaaaa.n5-macos-arm64.zip'
        self.client=FakeGitHub(nightly=True);self.add_macos(asset,cask='zerus@nightly');self.root.mkdir();self.run_mirror(True,'desktop/nightly')
        data=json.loads((self.root/'updates/v1/desktop/nightly.json').read_text())
        self.assertNotIn(asset,[row['name'] for row in data['artifacts']])
        self.assertFalse((self.root/'downloads/nightly-1234'/asset).exists())
        for cask in ('zerus',None):
            with self.subTest(cask=cask):
                self.client=FakeGitHub(nightly=True);self.add_macos(asset,cask=cask)
                with tempfile.TemporaryDirectory() as stage:
                    with self.assertRaises(ValueError):feed.validate_and_fetch(self.client,self.client.release,Path(stage),'desktop','nightly')
    def test_undeclared_or_foreign_macos_assets_are_rejected(self):
        for asset,declared in (('zerus-0.37.0-macos-arm64.zip',False),('zerus-0.37.0-macos-x86_64.zip',True)):
            with self.subTest(asset=asset,declared=declared):
                self.client=FakeGitHub();self.add_macos(asset,declared)
                with tempfile.TemporaryDirectory() as stage:
                    with self.assertRaises(ValueError):feed.validate_and_fetch(self.client,self.client.release,Path(stage),'desktop','stable')
    def test_signed_disk_images_stay_on_github_for_both_channels(self):
        for nightly in (False,True):
            with self.subTest(nightly=nightly):
                self.client=FakeGitHub(nightly=nightly);version=self.client.info['version']
                self.add_macos(f'zerus-{version}-macos-arm64.zip',cask='zerus@nightly' if nightly else 'zerus')
                image=f'zerus-{version}-macos-arm64.dmg'
                self.client.info['macos']['dmg']={'asset':image,'notarization_id':'1123abcd-4567-89ef-0123-456789abcdef'}
                self.client.payload[image]=b'notarized disk image';self.client.seal()
                channel='nightly' if nightly else 'stable'
                with tempfile.TemporaryDirectory() as stage:
                    result,files=feed.validate_and_fetch(self.client,self.client.release,Path(stage),'desktop',channel)
                    self.assertNotIn(image,files)
                    self.assertNotIn(image,self.client.calls)
                    self.assertNotIn(image,[row['name'] for row in result['artifacts']])
                    row=result['macos']['downloads'][1]
                    self.assertEqual(row['name'],image)
                    self.assertEqual(row['url'],self.client.release['html_url'].replace('/tag/','/download/')+'/'+image)
                    self.assertEqual(row['size'],len(self.client.payload[image]))
                row=next(row for row in self.client.release['assets'] if row['name']==image)
                row['digest']='sha256:'+'f'*64
                with tempfile.TemporaryDirectory() as stage:
                    with self.assertRaisesRegex(ValueError,'digest mismatch'):
                        feed.validate_and_fetch(self.client,self.client.release,Path(stage),'desktop',channel)
    def test_existing_mac_release_catalog_can_gain_only_its_additive_download_projection(self):
        self.add_macos();self.root.mkdir();self.run_mirror()
        path=self.root/'updates/v1/desktop/stable.json';current=json.loads(path.read_text())
        legacy={key:value for key,value in current.items() if key!='macos'}
        path.write_text(json.dumps(legacy));self.run_mirror()
        self.assertEqual(json.loads(path.read_text()),current)
        with self.assertRaises(ValueError):feed.check_previous(path,legacy)
        changed=copy.deepcopy(current);changed['macos']['downloads'][0]['sha256']='0'*64
        with self.assertRaises(ValueError):feed.check_previous(path,changed)
        path.write_text(json.dumps(legacy));changed=copy.deepcopy(current);changed['sourceCommit']='0'*40
        with self.assertRaises(ValueError):feed.check_previous(path,changed)
    def test_disk_image_metadata_cannot_admit_undeclared_or_foreign_assets(self):
        image='zerus-0.37.0-macos-arm64.dmg'
        for metadata in (None,{}, {'asset':'foreign.dmg','notarization_id':'1123abcd-4567-89ef-0123-456789abcdef'},
                         {'asset':image,'notarization_id':None}):
            with self.subTest(metadata=metadata):
                self.client=FakeGitHub();self.add_macos()
                if metadata is not None:self.client.info['macos']['dmg']=metadata
                self.client.payload[image]=b'disk image';self.client.seal()
                with tempfile.TemporaryDirectory() as stage:
                    with self.assertRaises(ValueError):
                        feed.validate_and_fetch(self.client,self.client.release,Path(stage),'desktop','stable')
    def test_modified_asset_hardfails_and_retains_feed(self):
        self.root.mkdir();self.run_mirror();path=self.root/'updates/v1/desktop/stable.json';before=path.read_bytes()
        asset=self.root/'downloads/v0.37.0/zerus-0.37.0-1-x86_64.pkg.tar.zst';asset.write_bytes(b'broken')
        with self.assertRaises(ValueError):self.run_mirror()
        self.assertEqual(path.read_bytes(),before)
    def test_digest_checksum_and_commit_mismatch(self):
        self.client.commit='c'*40
        with self.assertRaises(ValueError):self.run_mirror(False)
        self.client.commit=COMMIT;self.client.blobs[1]=b'mutated'
        with self.assertRaises(ValueError):self.run_mirror(False)
    def test_path_url_duplicate_and_missing_sums_rejected(self):
        for kind in ('path','url','duplicate','missing'):
            self.client=FakeGitHub();assets=self.client.release['assets']
            if kind=='path':assets[0]['name']='../secret'
            elif kind=='url':assets[0]['browser_download_url']='https://evil.example/package'
            elif kind=='duplicate':assets.append(copy.deepcopy(assets[0]))
            else:self.client.release['assets']=[a for a in assets if a['name']!='SHA256SUMS']
            with self.subTest(kind=kind),self.assertRaises(ValueError):self.run_mirror(False)
    def test_metadata_limit_and_disk_limit(self):
        self.client.release['assets'][0]['size']=feed.ASSET_LIMIT+1
        with self.assertRaises(ValueError):self.run_mirror(False)
        self.client=FakeGitHub()
        with self.assertRaises(ValueError):publisher.synchronize(self.client,self.root,['desktop/stable'],False,1)
        self.assertFalse(self.root.exists())
    def test_nightly_actual_provenance(self):
        self.client=FakeGitHub(nightly=True);self.run_mirror(False,'desktop/nightly')
        self.client.info['workflow_run_number']=6;self.client.seal()
        with self.assertRaises(ValueError):self.run_mirror(False,'desktop/nightly')
    def test_nightly_windows_zip_stays_on_github_and_out_of_the_v1_feed(self):
        self.client=FakeGitHub(nightly=True,windows=True);name=self.client.info['windows_portable']
        self.root.mkdir();self.run_mirror(True,'desktop/nightly')
        published=json.loads((self.root/'updates/v1/desktop/nightly.json').read_text())
        # Deployed GUIs reject unknown artifact kinds; the zip is neither listed nor mirrored.
        self.assertEqual({row['kind'] for row in published['artifacts']},{'linux-archive','arch-package'})
        self.assertNotIn(name,self.client.calls)
        self.assertFalse((self.root/'downloads/nightly-1234'/name).exists())
        self.assertTrue((self.root/'downloads/nightly-1234/SHA256SUMS').is_file())
    def test_stable_windows_zip_stays_on_github_and_out_of_the_v1_feed(self):
        self.client=FakeGitHub(windows=True);name=self.client.info['windows_portable']
        self.assertEqual(name,'zerus-0.37.0-windows-x86_64.zip')
        self.root.mkdir();self.run_mirror()
        published=json.loads((self.root/'updates/v1/desktop/stable.json').read_text())
        # The same projection as a stable release without it: deployed GUIs reject unknown kinds.
        self.assertEqual({row['kind'] for row in published['artifacts']},{'source','linux-archive','arch-package'})
        self.assertNotIn(name,[row['name'] for row in published['artifacts']])
        self.assertNotIn(name,self.client.calls)
        self.assertFalse((self.root/'downloads/v0.37.0'/name).exists())
        self.assertTrue((self.root/'downloads/v0.37.0/SHA256SUMS').is_file())
        # Retrying the same release changes nothing and downloads nothing.
        self.client.calls.clear();self.run_mirror();self.assertEqual(self.client.calls,[])
    def test_windows_installer_stays_on_github_with_exact_zip_binding_and_immutable_retry(self):
        for nightly in (False,True):
            with self.subTest(nightly=nightly):
                self.client=FakeGitHub(nightly=nightly,windows=True,installer=True)
                channel='nightly' if nightly else 'stable'
                tag=self.client.release['tag_name'];name=self.client.info['windows_installer']['asset']
                self.root.mkdir(exist_ok=True);self.run_mirror(True,'desktop/'+channel)
                published=json.loads((self.root/('updates/v1/desktop/'+channel+'.json')).read_text())
                self.assertNotIn(name,self.client.calls)
                self.assertNotIn(name,[row['name'] for row in published['artifacts']])
                self.assertFalse((self.root/'downloads'/tag/name).exists())
                self.client.calls.clear();self.run_mirror(True,'desktop/'+channel)
                self.assertEqual(self.client.calls,[])
                self.client.info['windows_installer']['portable_sha256']='b'*64;self.client.seal()
                with self.assertRaises(ValueError):self.run_mirror(True,'desktop/'+channel)
    def test_stable_feed_moves_from_a_release_without_the_zip_to_one_with_it(self):
        # 0.42.2 and earlier have no Windows zip; the release after it carries one.
        self.client=FakeGitHub(stable='0.42.2');self.root.mkdir();self.run_mirror();path=self.root/'updates/v1/desktop/stable.json'
        before=json.loads(path.read_text())
        self.client=FakeGitHub(windows=True,stable='0.42.3');self.client.release.update(id=11,published_at='2026-10-15T12:00:00Z');self.client.seal()
        self.run_mirror();after=json.loads(path.read_text())
        self.assertEqual((after['releaseId'],after['versionName']),(11,'0.42.3'))
        self.assertEqual([row['kind'] for row in after['artifacts']],[row['kind'] for row in before['artifacts']])
        self.assertFalse((self.root/'downloads/v0.42.3/zerus-0.42.3-windows-x86_64.zip').exists())
        # The older release still validates unchanged.
        self.client=FakeGitHub(stable='0.42.2')
        with tempfile.TemporaryDirectory() as stage:
            self.assertEqual(feed.validate_and_fetch(self.client,self.client.release,Path(stage),'desktop','stable')[0],before)
    def test_windows_zip_digest_and_family_are_still_verified(self):
        for nightly,selector in ((True,'desktop/nightly'),(False,'desktop/stable')):
            self.client=FakeGitHub(nightly=nightly,windows=True);name=self.client.info['windows_portable']
            row=next(asset for asset in self.client.release['assets'] if asset['name']==name);row['digest']='sha256:'+'0'*64
            with self.subTest(selector=selector),self.assertRaisesRegex(ValueError,'digest mismatch'):self.run_mirror(False,selector)
            for info in ({'windows_portable':'zerus-windows.zip'},{'windows_portable':None},
                         {'windows_portable':name.replace('-windows-','-windows-arm64-')}):
                # A renamed, undeclared or foreign zip fails closed and keeps the old catalog.
                self.client=FakeGitHub(nightly=nightly,windows=True);self.client.info.update(info);self.client.seal()
                with self.subTest(selector=selector,info=info),self.assertRaisesRegex(ValueError,'Unexpected'):self.run_mirror(False,selector)
    def test_android_dirty_source_required_same_code_replacement_forbidden(self):
        self.client=FakeGitHub(android=True);self.root.mkdir();self.run_mirror(True,'android/dev')
        self.client.release['id']=11;self.client.release['published_at']='2026-10-10T12:00:00Z'
        with self.assertRaises(ValueError):self.run_mirror(False,'android/dev')
        self.client.info.pop('source_snapshot');self.client.seal()
        with self.assertRaises(ValueError):self.run_mirror(False,'android/dev')
    def test_channel_mismatch_fetches_only_metadata(self):
        self.client=FakeGitHub(android=True);self.client.info['channel']='nightly';self.client.seal()
        with self.assertRaises(ValueError):self.run_mirror(False,'android/dev')
        self.assertEqual(set(self.client.calls),{'SHA256SUMS','release-info.json'})
    def test_corrupt_newest_does_not_silently_fall_back(self):
        self.client=FakeGitHub(android=True);self.client.rows=[self.client.release,copy.deepcopy(self.client.release)]
        self.client.rows[1]['id']=9;self.client.blobs[1]=b'corrupt'
        with self.assertRaises(ValueError):self.run_mirror(False,'android/dev')
    def test_malicious_aur_member_not_extracted(self):
        path=Path(self.temp.name)/'aur.tar.gz'
        with tarfile.open(path,'w:gz') as archive:
            member=tarfile.TarInfo('aur/zerus/.SRCINFO');member.type=tarfile.SYMTYPE;member.linkname='/etc/passwd';archive.addfile(member)
        with self.assertRaises(ValueError):feed.aur_versions(path)
    def test_symlink_root_and_feed_rejected(self):
        self.root.symlink_to(Path(self.temp.name),target_is_directory=True)
        with self.assertRaises(ValueError):feed.private_root(self.root)
    def test_http_untrusted_and_malformed_ports(self):
        for url in ('http://api.github.com','https://evil.example','https://'+urllib.parse.quote('user')+'@'+'api.github.com','https://api.github.com:bad','https://api.github.com/#x'):
            with self.subTest(url=url),self.assertRaises(ValueError):feed.https(url,{'api.github.com'})
    def test_asset_redirect_never_forwards_credentials(self):
        client=feed.GitHub('private-token');requests=[]
        class Response(io.BytesIO):
            headers={'Content-Length':'2'}
        class Opener:
            def open(self,request,timeout):
                requests.append(request)
                if len(requests)==1:
                    headers=Message();headers['Location']='https://release-assets.githubusercontent.com/asset'
                    raise urllib.error.HTTPError(request.full_url,302,'redirect',headers,None)
                return Response(b'ok')
        client.opener=Opener();self.assertEqual(b''.join(client.stream('https://api.github.com/repos/ufna/zerus/releases/assets/1',2,api=True,accept='application/octet-stream')),b'ok')
        self.assertEqual(requests[0].get_header('Authorization'),'Bearer private-token');self.assertIsNone(requests[1].get_header('Authorization'))
    def test_metadata_change_same_release_refused(self):
        self.root.mkdir();self.run_mirror();path=self.root/'updates/v1/desktop/stable.json';original=json.loads(path.read_text());changed={**original,'sourceCommit':'c'*40}
        with self.assertRaises(ValueError):feed.check_previous(path,changed)


class Interrupted(io.BytesIO):
    """A response that fails mid-body, as a dropped HTTP/1.1 connection does."""
    headers={}
    def read(self,size=-1):raise http.client.IncompleteRead(b'partial body',65536)


def interrupted(client):
    client.opener=type('Opener',(),{'open':lambda self,request,timeout:Interrupted()})()
    return client


class EntryPointFailureTest(unittest.TestCase):
    """Scheduled job logs carry one fixed line, never exception text or a traceback."""
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=Path(self.temp.name)/'content'
    def tearDown(self):self.temp.cleanup()
    def main(self,client=None,**patches):
        out,err=io.StringIO(),io.StringIO()
        with contextlib.ExitStack() as stack:
            stack.enter_context(patch.object(sys,'argv',['publisher','--root',str(self.root),'--channel','desktop/stable']))
            stack.enter_context(patch.object(publisher,'token',return_value='fixture-read-credential'))
            if client:stack.enter_context(patch.object(publisher,'GitHub',return_value=client))
            for name,value in patches.items():stack.enter_context(patch.object(publisher,name,side_effect=value))
            stack.enter_context(contextlib.redirect_stdout(out));stack.enter_context(contextlib.redirect_stderr(err))
            code=publisher.main()
        self.assertFalse(self.root.exists())
        return code,out.getvalue(),err.getvalue()
    def test_interrupted_github_body_reports_the_fixed_line(self):
        self.assertEqual(self.main(interrupted(feed.GitHub('fixture-read-credential'))),(1,'',FAILED))
        with self.assertRaisesRegex(ValueError,'GitHub transport interrupted'):
            b''.join(interrupted(feed.GitHub()).stream('https://api.github.com/repos/ufna/zerus/releases',2,api=True))
        class Opener:
            def open(self,request,timeout):raise http.client.BadStatusLine('fixture private status')
        client=feed.GitHub();client.opener=Opener()
        with self.assertRaisesRegex(ValueError,'GitHub transport unavailable'):client.api('/repos/ufna/zerus/releases')
    def test_truncated_or_malformed_aur_archive_reports_the_fixed_line(self):
        name='zerus-0.37.0-aur.tar.gz';valid=FakeGitHub().payload[name]
        for kind,data in (('truncated',valid[:len(valid)//2]),('malformed',gzip.compress(b'not a tar archive '*64)),
                          ('corrupt deflate',valid[:10]+b'\xff'*(len(valid)-10))):
            with self.subTest(kind=kind):
                # Sealed like any release: only parsing the archive itself can fail.
                client=FakeGitHub();client.payload[name]=data;client.seal()
                self.assertEqual(self.main(client),(1,'',FAILED))
                path=Path(self.temp.name)/'aur.tar.gz';path.write_bytes(data)
                with self.assertRaisesRegex(ValueError,'Invalid AUR metadata archive'):feed.aur_versions(path)
    def test_every_transport_and_archive_error_class_reports_the_fixed_line(self):
        detail='https://objects.example.invalid/signed?token=fixture-private-detail'
        for error in (http.client.IncompleteRead(detail.encode(),1),http.client.BadStatusLine(detail),
                      tarfile.ReadError(detail),EOFError(detail),zlib.error(detail),ValueError(detail)):
            with self.subTest(error=type(error).__name__):
                self.assertEqual(self.main(synchronize_channels=error),(1,'',FAILED))
        self.assertEqual(self.main(synchronize_channels=BlockingIOError(detail)),(1,'','Mirror already running\n'))
    def test_unexpected_defect_reports_only_its_class_and_interrupts_still_propagate(self):
        self.assertEqual(self.main(synchronize_channels=RuntimeError('fixture-private-detail')),
                         (1,'','Mirror failed unexpectedly (RuntimeError); previous catalogs retained\n'))
        for error in (KeyboardInterrupt(),SystemExit(3)):
            with self.subTest(error=type(error).__name__),self.assertRaises(type(error)):
                self.main(synchronize_channels=error)
    def test_python_older_than_311_is_refused_before_mirror_modules_load(self):
        out,err=io.StringIO(),io.StringIO()
        check=importlib.util.spec_from_file_location('old_python_publisher',SCRIPTS/'publish-update-feed.py')
        with patch.object(sys,'version_info',(3,10,14,'final',0)),patch.dict(sys.modules,{'update_feed':None}), \
             contextlib.redirect_stdout(out),contextlib.redirect_stderr(err),self.assertRaises(SystemExit) as refused:
            check.loader.exec_module(importlib.util.module_from_spec(check))
        self.assertEqual((refused.exception.code,out.getvalue(),err.getvalue()),
                         (1,'','Mirror requires Python 3.11 or newer; previous catalogs retained\n'))

if __name__=='__main__':unittest.main()
