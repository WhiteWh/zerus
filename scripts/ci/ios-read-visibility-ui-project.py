#!/usr/bin/env python3
"""Materialize the disposable simulator-only read-visibility project (zerus-a4mub.31), referencing unchanged
production sources: the conversation list's placement reporting and ZerusKit's reading rules.

This generator performs no build, simulator, keychain or network operation. OUTPUT must not exist.
The root-reviewed hosted gate supplies its private output directory; source identities are recorded.
"""
import hashlib
import json
from pathlib import Path
import sys


def quoted(value):
    return json.dumps(str(value))


def generate(root, output):
    root = Path(root).resolve(strict=True)
    output = Path(output)
    output.mkdir(mode=0o700)
    project = output / "ReadVisibilityUIFixture.xcodeproj"
    project.mkdir(mode=0o700)
    sources = ["mobile/ios/ReadVisibilityUIFixture/ReadVisibilityUIFixtureApp.swift",
               "mobile/ios/Zerus/Conversation/ConversationReadLayout.swift",
               "mobile/ios/ReadVisibilityUIFixture/ReadVisibilityUITests.swift"]
    # No copy, source rewriting, conditional product hook, live AppModel or app entry point.
    identities = {name: hashlib.sha256((root / name).read_bytes()).hexdigest() for name in sources}
    package = root / "mobile/ios/ZerusKit"
    identities.update({path.relative_to(root).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
                       for path in [package / "Package.swift", *sorted((package / "Sources/ZerusKit").glob("*.swift"))]})
    objects = []
    def object_(key, body):
        objects.append(f"{key} = {{ {body} }};")
    for index, name in enumerate(sources):
        object_(f"F{index:023d}", f"isa = PBXFileReference; lastKnownFileType = sourcecode.swift; path = {quoted(root / name)}; sourceTree = \"<absolute>\";")
        object_(f"B{index:023d}", f"isa = PBXBuildFile; fileRef = F{index:023d};")
    app = "A00000000000000000000001"
    tests = "A00000000000000000000002"
    project_id = "A00000000000000000000003"
    object_("A00000000000000000000004", "isa = PBXGroup; children = (" + ",".join(f"F{i:023d}" for i in range(len(sources))) + ",A00000000000000000000005); sourceTree = \"<group>\";")
    object_("A00000000000000000000005", "isa = PBXGroup; children = (A00000000000000000000006,A00000000000000000000007); name = Products; sourceTree = \"<group>\";")
    object_("A00000000000000000000006", "isa = PBXFileReference; explicitFileType = wrapper.application; path = ReadVisibilityUIFixture.app; sourceTree = BUILT_PRODUCTS_DIR;")
    object_("A00000000000000000000007", "isa = PBXFileReference; explicitFileType = wrapper.cfbundle; path = ReadVisibilityUIFixtureTests.xctest; sourceTree = BUILT_PRODUCTS_DIR;")
    object_("A00000000000000000000008", "isa = PBXSourcesBuildPhase; buildActionMask = 2147483647; files = (B00000000000000000000000,B00000000000000000000001); runOnlyForDeploymentPostprocessing = 0;")
    object_("A00000000000000000000009", "isa = PBXSourcesBuildPhase; buildActionMask = 2147483647; files = (B00000000000000000000002); runOnlyForDeploymentPostprocessing = 0;")
    object_("A00000000000000000000010", "isa = XCLocalSwiftPackageReference; relativePath = " + quoted(package) + ";")
    object_("A00000000000000000000011", "isa = XCSwiftPackageProductDependency; package = A00000000000000000000010; productName = ZerusKit;")
    object_("A00000000000000000000012", "isa = PBXBuildFile; productRef = A00000000000000000000011;")
    object_("A00000000000000000000013", "isa = PBXFrameworksBuildPhase; buildActionMask = 2147483647; files = (A00000000000000000000012); runOnlyForDeploymentPostprocessing = 0;")
    object_("A00000000000000000000014", "isa = PBXFrameworksBuildPhase; buildActionMask = 2147483647; files = (); runOnlyForDeploymentPostprocessing = 0;")
    object_("A00000000000000000000015", f"isa = PBXContainerItemProxy; containerPortal = {project_id}; proxyType = 1; remoteGlobalIDString = {app}; remoteInfo = ReadVisibilityUIFixture;")
    object_("A00000000000000000000016", f"isa = PBXTargetDependency; target = {app}; targetProxy = A00000000000000000000015;")
    settings = 'CODE_SIGNING_ALLOWED = NO; CODE_SIGNING_REQUIRED = NO; CODE_SIGN_IDENTITY = ""; SDKROOT = iphonesimulator; SUPPORTED_PLATFORMS = iphonesimulator; ONLY_ACTIVE_ARCH = YES; IPHONEOS_DEPLOYMENT_TARGET = 18.0; SWIFT_VERSION = 6.0; SWIFT_OPTIMIZATION_LEVEL = "-Onone"; GENERATE_INFOPLIST_FILE = YES; TARGETED_DEVICE_FAMILY = "1,2";'
    object_("A00000000000000000000017", f'isa = XCBuildConfiguration; name = Debug; buildSettings = {{ {settings} PRODUCT_NAME = ReadVisibilityUIFixture; PRODUCT_BUNDLE_IDENTIFIER = dev.zerus.fixture.readvisibility; INFOPLIST_KEY_UIApplicationSceneManifest_Generation = YES; INFOPLIST_KEY_UILaunchScreen_Generation = YES; }};')
    object_("A00000000000000000000018", f'isa = XCBuildConfiguration; name = Debug; buildSettings = {{ {settings} PRODUCT_NAME = ReadVisibilityUIFixtureTests; PRODUCT_BUNDLE_IDENTIFIER = dev.zerus.fixture.readvisibility.uitests; TEST_TARGET_NAME = ReadVisibilityUIFixture; }};')
    object_("A00000000000000000000019", "isa = XCBuildConfiguration; name = Debug; buildSettings = { CLANG_ENABLE_MODULES = YES; }; ")
    for key, config in (("20", "17"), ("21", "18"), ("22", "19")):
        object_("A000000000000000000000" + key, f"isa = XCConfigurationList; buildConfigurations = (A000000000000000000000{config}); defaultConfigurationIsVisible = 0; defaultConfigurationName = Debug;")
    object_(app, 'isa = PBXNativeTarget; name = ReadVisibilityUIFixture; productName = ReadVisibilityUIFixture; productReference = A00000000000000000000006; productType = "com.apple.product-type.application"; buildConfigurationList = A00000000000000000000020; buildPhases = (A00000000000000000000008,A00000000000000000000013); dependencies = (); packageProductDependencies = (A00000000000000000000011);')
    object_(tests, 'isa = PBXNativeTarget; name = ReadVisibilityUIFixtureTests; productName = ReadVisibilityUIFixtureTests; productReference = A00000000000000000000007; productType = "com.apple.product-type.bundle.ui-testing"; buildConfigurationList = A00000000000000000000021; buildPhases = (A00000000000000000000009,A00000000000000000000014); dependencies = (A00000000000000000000016);')
    object_(project_id, f'isa = PBXProject; attributes = {{ LastUpgradeCheck = 2600; TargetAttributes = {{ {tests} = {{ TestTargetID = {app}; }}; }}; }}; buildConfigurationList = A00000000000000000000022; compatibilityVersion = "Xcode 15.0"; developmentRegion = en; knownRegions = (en,Base); mainGroup = A00000000000000000000004; productRefGroup = A00000000000000000000005; projectDirPath = ""; projectRoot = ""; packageReferences = (A00000000000000000000010); targets = ({app},{tests});')
    (project / "project.pbxproj").write_text("// !$*UTF8*$!\n{ archiveVersion = 1; classes = {}; objectVersion = 60; objects = {\n" + "\n".join(objects) + f"\n}}; rootObject = {project_id}; }}\n")
    schemes = project / "xcshareddata/xcschemes"
    schemes.mkdir(parents=True)
    def reference(identifier, name, product):
        return f'<BuildableReference BuildableIdentifier="primary" BlueprintIdentifier="{identifier}" BuildableName="{product}" BlueprintName="{name}" ReferencedContainer="container:ReadVisibilityUIFixture.xcodeproj"/>'
    app_ref = reference(app, "ReadVisibilityUIFixture", "ReadVisibilityUIFixture.app")
    test_ref = reference(tests, "ReadVisibilityUIFixtureTests", "ReadVisibilityUIFixtureTests.xctest")
    (schemes / "ReadVisibilityUIFixture.xcscheme").write_text(f'''<?xml version="1.0" encoding="UTF-8"?>
<Scheme version="1.7" LastUpgradeVersion="2600">
 <BuildAction parallelizeBuildables="NO" buildImplicitDependencies="YES"><BuildActionEntries>
  <BuildActionEntry buildForTesting="YES" buildForRunning="YES" buildForProfiling="NO" buildForArchiving="NO" buildForAnalyzing="YES">{app_ref}</BuildActionEntry>
  <BuildActionEntry buildForTesting="YES" buildForRunning="NO" buildForProfiling="NO" buildForArchiving="NO" buildForAnalyzing="YES">{test_ref}</BuildActionEntry>
 </BuildActionEntries></BuildAction>
 <TestAction buildConfiguration="Debug" selectedDebuggerIdentifier="" selectedLauncherIdentifier="Xcode.IDEFoundation.Launcher.PosixSpawn" shouldUseLaunchSchemeArgsEnv="YES"><Testables><TestableReference skipped="NO" parallelizable="NO">{test_ref}</TestableReference></Testables></TestAction>
 <LaunchAction buildConfiguration="Debug" selectedDebuggerIdentifier="" selectedLauncherIdentifier="Xcode.IDEFoundation.Launcher.PosixSpawn"><BuildableProductRunnable runnableDebuggingMode="0">{app_ref}</BuildableProductRunnable></LaunchAction>
</Scheme>
''')
    (output / "source-identities.json").write_text(json.dumps({"sources": identities, "scope": "production placement reporting and ZerusKit reading rules in a synthetic list; not the live AppModel or relay"}, indent=2, sort_keys=True) + "\n")
    return project


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: ios-read-visibility-ui-project.py ABSENT_PRIVATE_OUTPUT")
    generate(Path(__file__).resolve().parents[2], sys.argv[1])
