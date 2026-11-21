package org.bukkit;

import java.util.Collection;
import java.util.List;
import java.util.UUID;
import java.util.logging.Logger;

import org.bukkit.command.CommandSender;
import org.bukkit.command.ConsoleCommandSender;
import org.bukkit.entity.Player;
import org.bukkit.plugin.PluginManager;
import org.bukkit.scheduler.BukkitScheduler;

public interface Server {
    String getName();
    String getVersion();
    String getBukkitVersion();
    Collection<? extends Player> getOnlinePlayers();
    int getMaxPlayers();
    String getMotd();
    Player getPlayer(String name);
    Player getPlayerExact(String name);
    Player getPlayer(UUID id);
    int broadcastMessage(String message);
    PluginManager getPluginManager();
    BukkitScheduler getScheduler();
    Logger getLogger();
    List<World> getWorlds();
    World getWorld(String name);
    ConsoleCommandSender getConsoleSender();
    boolean dispatchCommand(CommandSender sender, String commandLine);
    boolean isPrimaryThread();
}
