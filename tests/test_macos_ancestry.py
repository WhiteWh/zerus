"""Hosted diagnostic eligibility cannot widen the production Mac rule."""
import unittest
from macos_ancestry import unsupported_hosted_foreign


class MacosAncestryDiagnostic(unittest.TestCase):
    def test_only_an_observed_unsupported_foreign_node_on_github_hosted_macos_can_skip(self):
        hosted = dict(GITHUB_ACTIONS='true', RUNNER_ENVIRONMENT='github-hosted', RUNNER_OS='macOS')
        own = dict(pid=20, uid=1000, path='/private/example/hgs')
        login = dict(pid=10, uid=0, path='/usr/bin/login')
        init = dict(pid=1, uid=0, path='/sbin/launchd')
        foreign = dict(pid=2, uid=0, path='/usr/libexec/sshd-session')
        self.assertIsNone(unsupported_hosted_foreign([own, login, init], 1000, hosted))
        self.assertIsNone(unsupported_hosted_foreign([own, init], 1000, hosted))
        self.assertEqual(unsupported_hosted_foreign([own, foreign, init], 1000, hosted), foreign)
        for env in ({}, dict(hosted, RUNNER_ENVIRONMENT='self-hosted'), dict(hosted, RUNNER_OS='Linux')):
            self.assertIsNone(unsupported_hosted_foreign([own, foreign, init], 1000, env))
        # An own-user agent is never a reason to skip its refusal test.
        agent = dict(pid=3, uid=1000, path='/private/example/claude')
        self.assertIsNone(unsupported_hosted_foreign([own, agent, init], 1000, hosted))


if __name__ == '__main__':
    unittest.main()
