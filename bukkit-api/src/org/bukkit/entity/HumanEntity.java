package org.bukkit.entity;

import org.bukkit.GameMode;
import org.bukkit.inventory.PlayerInventory;

public interface HumanEntity extends LivingEntity {
    PlayerInventory getInventory();
    GameMode getGameMode();
    void setGameMode(GameMode mode);
}
