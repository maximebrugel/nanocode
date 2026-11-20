import { promises } from "node:fs";

export default promises;
export { FileHandle } from "node:fs";
export const { access, stat, lstat, readFile, writeFile, appendFile, mkdir, mkdtemp, readdir, rmdir, rm, unlink, rename, copyFile, cp, truncate, utimes, chmod, chown, realpath, readlink, link, lutimes, lchmod, lchown, symlink, open, constants } = promises;
