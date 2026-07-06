import { describe, expect, it } from '../test-shim';
import { gitUrlScheme, looksLikeGitUrl } from './giturl';

// The connect modal shares one box with ASP tickets/node-ids, so the classifier
// must accept every git-remote shape yet never misfire on the two ASP inputs.
describe('looksLikeGitUrl', () => {
  const gitUrls: [string, 'https' | 'ssh'][] = [
    ['https://github.com/o/r.git', 'https'],
    ['https://github.com/o/r', 'https'],
    ['https://github.com/o/r/', 'https'],
    ['https://gitlab.example.com:8443/g/p.git', 'https'],
    ['  https://github.com/o/r.git  ', 'https'], // trimmed
    ['ssh://git@github.com/o/r.git', 'ssh'],
    ['ssh://git@github.com:2222/o/r.git', 'ssh'],
    ['git@github.com:o/r.git', 'ssh'], // scp-like
    ['git@github.com:o/r', 'ssh'],
    ['user@git.example.com:group/project.git', 'ssh'],
    ['/srv/repos/thing.git', 'ssh'], // bare local path ending .git
    ['relative/path/to/repo.git', 'ssh'],
  ];
  for (const [input, scheme] of gitUrls) {
    it(`accepts ${JSON.stringify(input)} as ${scheme}`, () => {
      expect(looksLikeGitUrl(input)).toBe(true);
      expect(gitUrlScheme(input)).toBe(scheme);
    });
  }

  const notGit: string[] = [
    '', '   ',
    // A 64-hex node id (ASP peer) — no scheme, no scp colon, no .git.
    'a'.repeat(64),
    'deadbeef'.repeat(8),
    // An iroh ticket blob (long base32; representative shape).
    'nodeaaajmb2i5jbmxlptn5vtlybudmkwwuxlg35a5nu4wqjm7pbxhr6qcaibahaqcbs',
    'just some plain text',
    'http://insecure.example.com/o/r.git', // http:// is rejected (https only)
    'git://github.com/o/r.git', // git:// scheme rejected
    'file:///tmp/repo', // file:// rejected
    '12:34', // time-like, host not hostish and no user
    'word:word', // bare word:word, not hostish
    'C:\\Users\\me\\notes', // windows path, not scp
    'README', // plain word
    'my-vault',
  ];
  for (const input of notGit) {
    it(`rejects ${JSON.stringify(input)}`, () => {
      expect(looksLikeGitUrl(input)).toBe(false);
      expect(gitUrlScheme(input)).toBeNull();
    });
  }
});
