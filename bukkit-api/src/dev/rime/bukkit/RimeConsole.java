package dev.rime.bukkit;

import org.bukkit.Server;
import org.bukkit.command.ConsoleCommandSender;

public final class RimeConsole implements ConsoleCommandSender {
    public static final RimeConsole INSTANCE = new RimeConsole();
    @Override public void sendMessage(String message) { Bridge.log(0, org.bukkit.ChatColor.stripColor(message)); }
    @Override public void sendMessage(String... messages) { for (String m : messages) sendMessage(m); }
    @Override public String getName() { return "CONSOLE"; }
    @Override public boolean isOp() { return true; }
    @Override public boolean hasPermission(String permission) { return true; }
    @Override public Server getServer() { return RimeServer.INSTANCE; }
}
