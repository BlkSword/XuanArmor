#!/bin/bash
# XuanArmor 卸载脚本
set -e

echo "停止并禁用 XuanArmor 服务..."
systemctl stop xuanarmor 2>/dev/null || true
systemctl disable xuanarmor 2>/dev/null || true

echo "移除 systemd 服务..."
rm -f /etc/systemd/system/xuanarmor.service
systemctl daemon-reload

echo "移除二进制..."
rm -f /usr/local/bin/xuanarmor

echo "是否删除配置文件 /etc/xuanarmor？（y/N）"
read -r answer
if [ "$answer" = "y" ] || [ "$answer" = "Y" ]; then
    rm -rf /etc/xuanarmor
fi

echo "✓ XuanArmor 已卸载"
