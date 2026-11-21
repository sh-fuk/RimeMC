package org.bukkit.block;

import org.bukkit.Location;
import org.bukkit.Material;
import org.bukkit.World;

public interface Block {
    Material getType();
    void setType(Material type);
    int getX();
    int getY();
    int getZ();
    World getWorld();
    Location getLocation();
}
