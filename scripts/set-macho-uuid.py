# 把 Mach-O 可执行文件的 LC_UUID 改成按架构定的固定值，改完要重新签名。
#
# 用法：python3 -I set-macho-uuid.py <可执行文件> <架构>（lipo -archs 报的 arm64、x86_64）
#
# macOS 的本地网络权限按责任进程程序里的 Mach-O UUID 认它是不是这个 app。shell 的责任进程是拉起它的
# 宿主，升级后宿主还是旧版的程序，UUID 换了的话，接手过来的终端连局域网一律被拦（No route to host），
# 放到新位置还会再弹权限框。所以每个版本都写同一个 UUID，旧宿主照样被认作这个 app。发布构建去掉了
# 调试信息、不出 dSYM，崩溃报告不靠 UUID 找符号。
import struct
import sys
import uuid

path, arch = sys.argv[1:]
data = bytearray(open(path, "rb").read())
if struct.unpack_from("<I", data)[0] != 0xFEEDFACF:
    sys.exit(f"{path}: 不是单一架构的 64 位 Mach-O")
offset = 32
for _ in range(struct.unpack_from("<I", data, 16)[0]):
    command, size = struct.unpack_from("<II", data, offset)
    if command == 0x1B:  # LC_UUID
        data[offset + 8 : offset + 24] = uuid.uuid5(uuid.NAMESPACE_DNS, f"{arch}.runode.dev").bytes
        open(path, "wb").write(data)
        break
    offset += size
else:
    sys.exit(f"{path}: 没有 LC_UUID")
