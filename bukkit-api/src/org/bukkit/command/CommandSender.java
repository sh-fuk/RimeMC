package org.bukkit.command;

import org.bukkit.Server;

public interface CommandSender {
    void sendMessage(String message);
    void sendMessage(String... messages);
    String getName();
    boolean isOp();
    boolean hasPermission(String permission);
    Server getServer();
}
