package org.bukkit;

import java.util.List;

import org.bukkit.block.Block;
import org.bukkit.entity.Player;

public interface World {
    String getName();
    Block getBlockAt(int x, int y, int z);
    Block getBlockAt(Location location);
    long getTime();
    void setTime(long time);
    long getFullTime();
    List<Player> getPlayers();
    Location getSpawnLocation();
}
