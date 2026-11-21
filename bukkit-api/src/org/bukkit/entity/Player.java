package org.bukkit.entity;

import org.bukkit.OfflinePlayer;

public interface Player extends HumanEntity, OfflinePlayer {
    String getDisplayName();
    void setDisplayName(String name);
    void kickPlayer(String message);
    boolean performCommand(String command);
    void chat(String msg);
}
