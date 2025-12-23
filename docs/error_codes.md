# Complete Solana Protocol Error Reference

Reference for error codes encountered when parsing transactions and submitting bundles. These errors come from on-chain DEX programs and are useful for debugging failed swaps and bundle rejections.

## Error Code Ranges Overview

| Range | Hex Range | Category |
|-------|-----------|----------|
| **0 - 999** | `0x0 - 0x3E7` | Non-Anchor programs (Token Program, Raydium AMM V4) |
| **100 - 199** | `0x64 - 0xC7` | Anchor InstructionError range |
| **300 - 399** | `0x12C - 0x18F` | Anchor InstructionError range |
| **2000 - 2999** | `0x7D0 - 0xBB7` | Anchor ConstraintError range |
| **3000 - 3999** | `0xBB8 - 0xF9F` | Anchor AccountError range |
| **4100 - 4199** | `0x1004 - 0x1067` | Anchor RequireError range |
| **5000 - 5999** | `0x1388 - 0x176F` | Anchor TypesError range |
| **6000+** | `0x1770+` | Custom program errors (Anchor `#[error_code]`) |

---

## Program IDs for Error Attribution

| Program | Program ID |
|---------|------------|
| Orca Whirlpool | `whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc` |
| Raydium CLMM | `CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK` |
| Raydium AMM V4 | `675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8` |
| Meteora DLMM | `LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo` |
| Meteora DAMM v2 / CP | `cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG` |
| Meteora CP (old) | `Eo7WjKq67rjJQSZxS6z3YkapzY3eMj6Xy8X5EQVn5UaB` |
| Pancake CLMM | `HpNfyc2Saw7RKkQd8nEL4khUcuPhQ7WwY1B2qjx8jxFq` |
| Token Program | `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA` |
| Token-2022 | `TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb` |
| Solend | `So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo` |

---

## Your Observed Errors (from out.txt)

| Error Hex | Decimal | Count | Possible Sources |
|-----------|---------|-------|------------------|
| **0x4** | 4 | 62 | Raydium V4: `InvalidCoinVault` OR Token: `OwnerMismatch` |
| **0x1772** | 6002 | 46 | ⚠️ COLLISION - see below |
| **0x28** | 40 | 18 | Raydium V4: `InsufficientFunds` |
| **0x177d** | 6013 | 14 | ⚠️ COLLISION - see below |
| **0x7d3** | 2003 | 10 | Anchor: `ConstraintRaw` |
| **0xbbd** | 3005 | 6 | Anchor: `AccountNotEnoughKeys` |
| **0x1** | 1 | 6 | Token: `InsufficientFunds` OR Raydium V4: `InvalidProgramAddress` |

---

## ⚠️ ERROR CODE COLLISIONS

### 0x1772 (6002) - FIVE DIFFERENT MEANINGS

| Protocol | Error Name | Description |
|----------|------------|-------------|
| **Orca Whirlpool** | `TickArrayExistInPool` | Tick array already exists in pool |
| **Meteora DLMM** | `InvalidInput` | Invalid input data |
| **Raydium CLMM** | `InvalidUpdateConfigFlag` | Invalid update config flag |
| **Pancake CLMM** | `InvalidUpdateConfigFlag` | Invalid update amm config flag |
| **Meteora DAMM v2** | `ExceededSlippage` | **Exceeded slippage tolerance** |

### 0x177d (6013) - FIVE DIFFERENT MEANINGS

| Protocol | Error Name | Description |
|----------|------------|-------------|
| **Orca Whirlpool** | `LiquidityTooHigh` | Liquidity too high |
| **Meteora DLMM** | `InvalidFeeOwner` | Invalid fee owner |
| **Raydium CLMM** | `PriceSlippageCheck` | Price slippage check failed |
| **Pancake CLMM** | `SqrtPriceLimitOverflow` | Square root price limit overflow |
| **Meteora DAMM v2** | `InvalidFeeCurve` | Invalid fee curve |

### 0x1770 (6000) - FIVE DIFFERENT MEANINGS

| Protocol | Error Name | Description |
|----------|------------|-------------|
| **Orca Whirlpool** | `InvalidEnum` | Invalid enum value |
| **Meteora DLMM** | `InvalidStartBinIndex` | Invalid start bin index |
| **Raydium CLMM** | `LOK` | Lock error (re-entrancy) |
| **Pancake CLMM** | `LOK` | LOK (re-entrancy lock) |
| **Meteora DAMM v2** | `MathOverflow` | Math operation overflow |

### 0x1771 (6001) - FIVE DIFFERENT MEANINGS

| Protocol | Error Name | Description |
|----------|------------|-------------|
| **Orca Whirlpool** | `InvalidStartTick` | Invalid start tick |
| **Meteora DLMM** | `InvalidBinId` | Invalid bin id |
| **Raydium CLMM** | `NotApproved` | Not approved |
| **Pancake CLMM** | `NotApproved` | Not approved |
| **Meteora DAMM v2** | `InvalidFee` | Invalid fee setup |

### 0x1773 (6003) - FIVE DIFFERENT MEANINGS

| Protocol | Error Name | Description |
|----------|------------|-------------|
| **Orca Whirlpool** | `TickArrayIndexOutofBounds` | Tick array index out of bounds |
| **Meteora DLMM** | `ExceededAmountSlippageTolerance` | Exceeded amount slippage tolerance |
| **Raydium CLMM** | `AccountLack` | Missing account |
| **Pancake CLMM** | `AccountLack` | Account lack |
| **Meteora DAMM v2** | `PoolDisabled` | Pool disabled |

### 0x1774 (6004) - FIVE DIFFERENT MEANINGS

| Protocol | Error Name | Description |
|----------|------------|-------------|
| **Orca Whirlpool** | `InvalidTickSpacing` | Invalid tick spacing |
| **Meteora DLMM** | `ExceededBinSlippageTolerance` | Exceeded bin slippage tolerance |
| **Raydium CLMM** | `InvaildPoolMint` | Invalid pool mint |
| **Pancake CLMM** | `ClosePositionErr` | Remove liquidity first |
| **Meteora DAMM v2** | `ExceedMaxFeeBps` | Exceeded max fee bps |

### 0x4 (4) - TWO DIFFERENT MEANINGS

| Protocol | Error Name | Description |
|----------|------------|-------------|
| **Raydium AMM V4** | `InvalidCoinVault` | Invalid coin vault |
| **Token Program** | `OwnerMismatch` | Owner mismatch |

### 0x1 (1) - TWO DIFFERENT MEANINGS

| Protocol | Error Name | Description |
|----------|------------|-------------|
| **Token Program** | `InsufficientFunds` | Insufficient funds |
| **Raydium AMM V4** | `InvalidProgramAddress` | Invalid program address |

---

## Anchor Framework Errors (All Anchor Programs)

**Source:** [solana-foundation/anchor - lang/src/error.rs](https://github.com/solana-foundation/anchor/blob/main/lang/src/error.rs)

**Note:** All programs using Anchor inherit these error codes. Custom program errors start at 6000+.

### Constraint Errors (2000-2047 / 0x7D0-0x7FF)

| Code | Hex | Name | Description |
|------|-----|------|-------------|
| 2000 | 0x7D0 | ConstraintMut | Account mutability constraint violation |
| 2001 | 0x7D1 | ConstraintHasOne | Has-one constraint violation |
| 2002 | 0x7D2 | ConstraintSigner | Signer constraint violation |
| **2003** | **0x7D3** | **ConstraintRaw** | **Raw constraint violation (custom condition failed)** |
| 2004 | 0x7D4 | ConstraintOwner | Owner constraint violation |
| 2005 | 0x7D5 | ConstraintRentExempt | Rent exempt constraint violation |
| 2006 | 0x7D6 | ConstraintSeeds | PDA seeds constraint violation |
| 2007 | 0x7D7 | ConstraintExecutable | Executable constraint violation |
| 2008 | 0x7D8 | ConstraintState | State constraint violation (deprecated) |
| 2009 | 0x7D9 | ConstraintAssociated | Associated token constraint violation |
| 2010 | 0x7DA | ConstraintAssociatedInit | Associated init constraint violation |
| 2011 | 0x7DB | ConstraintClose | Close constraint violation |
| 2012 | 0x7DC | ConstraintAddress | Address constraint violation |
| 2013 | 0x7DD | ConstraintZero | Zero constraint violation |
| 2014 | 0x7DE | ConstraintTokenMint | Token mint constraint violation |
| 2015 | 0x7DF | ConstraintTokenOwner | Token owner constraint violation |
| 2016 | 0x7E0 | ConstraintMintMintAuthority | Mint authority constraint violation |
| 2017 | 0x7E1 | ConstraintMintFreezeAuthority | Freeze authority constraint violation |
| 2018 | 0x7E2 | ConstraintMintDecimals | Mint decimals constraint violation |
| 2019 | 0x7E3 | ConstraintSpace | Space constraint violation |
| 2020 | 0x7E4 | ConstraintAccountIsNone | Account is none when expected some |
| 2021 | 0x7E5 | ConstraintTokenTokenProgram | Token program constraint violation |
| 2022 | 0x7E6 | ConstraintMintTokenProgram | Mint token program constraint violation |
| 2023 | 0x7E7 | ConstraintAssociatedTokenTokenProgram | Associated token program constraint violation |
| 2024 | 0x7E8 | ConstraintMintGroupPointerExtension | Mint group pointer extension constraint |
| 2025 | 0x7E9 | ConstraintMintGroupPointerExtensionAuthority | Group pointer extension authority constraint |
| 2026 | 0x7EA | ConstraintMintGroupPointerExtensionGroupAddress | Group pointer extension group address constraint |
| 2027 | 0x7EB | ConstraintMintGroupMemberPointerExtension | Group member pointer extension constraint |
| 2028 | 0x7EC | ConstraintMintGroupMemberPointerExtensionAuthority | Group member pointer extension authority constraint |
| 2029 | 0x7ED | ConstraintMintGroupMemberPointerExtensionMemberAddress | Group member pointer extension member address constraint |
| 2030 | 0x7EE | ConstraintMintMetadataPointerExtension | Metadata pointer extension constraint |
| 2031 | 0x7EF | ConstraintMintMetadataPointerExtensionAuthority | Metadata pointer extension authority constraint |
| 2032 | 0x7F0 | ConstraintMintMetadataPointerExtensionMetadataAddress | Metadata pointer extension metadata address constraint |
| 2033 | 0x7F1 | ConstraintMintCloseAuthorityExtension | Close authority extension constraint |
| 2034 | 0x7F2 | ConstraintMintCloseAuthorityExtensionAuthority | Close authority extension authority constraint |
| 2035 | 0x7F3 | ConstraintMintPermanentDelegateExtension | Permanent delegate extension constraint |
| 2036 | 0x7F4 | ConstraintMintPermanentDelegateExtensionDelegate | Permanent delegate extension delegate constraint |
| 2037 | 0x7F5 | ConstraintMintTransferHookExtension | Transfer hook extension constraint |
| 2038 | 0x7F6 | ConstraintMintTransferHookExtensionAuthority | Transfer hook extension authority constraint |
| 2039 | 0x7F7 | ConstraintMintTransferHookExtensionProgramId | Transfer hook extension program id constraint |
| 2040 | 0x7F8 | Ed25519InvalidProgram | Invalid Ed25519 program id for signature verification |
| 2041 | 0x7F9 | Secp256k1InvalidProgram | Invalid Secp256k1 program id for signature verification |
| 2042 | 0x7FA | InstructionHasAccounts | Instruction unexpectedly had account metas |
| 2043 | 0x7FB | MessageTooLong | Message length exceeds allowed maximum |
| 2045 | 0x7FD | InvalidRecoveryId | Invalid Secp256k1 recovery id (must be 0 or 1) |
| 2047 | 0x7FF | SignatureVerificationFailed | Signature verification failed |

### Require Errors (2500-2506 / 0x9C4-0x9CA)

| Code | Hex | Name | Description |
|------|-----|------|-------------|
| 2500 | 0x9C4 | RequireViolated | A require expression was violated |
| 2501 | 0x9C5 | RequireEqViolated | A require_eq expression was violated |
| 2502 | 0x9C6 | RequireKeysEqViolated | A require_keys_eq expression was violated |
| 2503 | 0x9C7 | RequireNeqViolated | A require_neq expression was violated |
| 2504 | 0x9C8 | RequireKeysNeqViolated | A require_keys_neq expression was violated |
| 2505 | 0x9C9 | RequireGtViolated | A require_gt expression was violated |
| 2506 | 0x9CA | RequireGteViolated | A require_gte expression was violated |

### Account Errors (3000-3017 / 0xBB8-0xBC9)

| Code | Hex | Name | Description |
|------|-----|------|-------------|
| 3000 | 0xBB8 | AccountDiscriminatorAlreadySet | Discriminator already set |
| 3001 | 0xBB9 | AccountDiscriminatorNotFound | Discriminator not found |
| 3002 | 0xBBA | AccountDiscriminatorMismatch | Discriminator mismatch |
| 3003 | 0xBBB | AccountDidNotDeserialize | Failed to deserialize account |
| 3004 | 0xBBC | AccountDidNotSerialize | Failed to serialize account |
| **3005** | **0xBBD** | **AccountNotEnoughKeys** | **Not enough account keys provided** |
| 3006 | 0xBBE | AccountNotMutable | Account not mutable |
| 3007 | 0xBBF | AccountOwnedByWrongProgram | Owned by wrong program |
| 3008 | 0xBC0 | InvalidProgramId | Invalid program ID |
| 3009 | 0xBC1 | InvalidProgramExecutable | Invalid program executable |
| 3010 | 0xBC2 | AccountNotSigner | Account not signer |
| 3011 | 0xBC3 | AccountNotSystemOwned | Not system owned |
| 3012 | 0xBC4 | AccountNotInitialized | Account not initialized |
| 3013 | 0xBC5 | AccountNotProgramData | Not program data |
| 3014 | 0xBC6 | AccountNotAssociatedTokenAccount | Not associated token account |
| 3015 | 0xBC7 | AccountSysvarMismatch | Sysvar mismatch |
| 3016 | 0xBC8 | AccountReallocExceedsLimit | Realloc exceeds limit |
| 3017 | 0xBC9 | AccountDuplicateReallocs | Duplicate reallocs |

---

## Orca Whirlpool Errors (6000+ / 0x1770+)

**Source: [orca-so/whirlpools programs/whirlpool/src/errors.rs](https://github.com/orca-so/whirlpools/blob/main/programs/whirlpool/src/errors.rs)**

| Code | Hex | Name | Description |
|------|-----|------|-------------|
| 6000 | 0x1770 | InvalidEnum | Invalid enum value |
| 6001 | 0x1771 | InvalidStartTick | Invalid start tick |
| 6002 | 0x1772 | TickArrayExistInPool | Tick array already exists in pool |
| 6003 | 0x1773 | TickArrayIndexOutofBounds | Tick array index out of bounds |
| 6004 | 0x1774 | InvalidTickSpacing | Invalid tick spacing |
| 6005 | 0x1775 | ClosePositionNotEmpty | Cannot close non-empty position |
| 6006 | 0x1776 | DivideByZero | Division by zero |
| 6007 | 0x1777 | NumberCastError | Number cast error |
| 6008 | 0x1778 | NumberDownCastError | Number downcast error |
| 6009 | 0x1779 | TickNotFound | Tick not found |
| 6010 | 0x177a | InvalidTickIndex | Invalid tick index |
| 6011 | 0x177b | SqrtPriceOutOfBounds | Sqrt price out of bounds |
| 6012 | 0x177c | LiquidityZero | Liquidity is zero |
| 6013 | 0x177d | LiquidityTooHigh | Liquidity too high |
| 6014 | 0x177e | LiquidityOverflow | Liquidity overflow |
| 6015 | 0x177f | LiquidityUnderflow | Liquidity underflow |
| 6016 | 0x1780 | LiquidityNetError | Liquidity net error |
| 6017 | 0x1781 | TokenMaxExceeded | Token max exceeded |
| 6018 | 0x1782 | TokenMinSubceeded | Token min not met |
| 6019 | 0x1783 | MissingOrInvalidDelegate | Missing or invalid delegate |
| 6020 | 0x1784 | InvalidPositionTokenAmount | Invalid position token amount |
| 6021 | 0x1785 | InvalidTimestampConversion | Invalid timestamp conversion |
| 6022 | 0x1786 | InvalidTimestamp | Invalid timestamp |
| 6023 | 0x1787 | InvalidTickArraySequence | Invalid tick array sequence |
| 6024 | 0x1788 | InvalidTokenMintOrder | Invalid token mint order |
| 6025 | 0x1789 | RewardNotInitialized | Reward not initialized |
| 6026 | 0x178a | InvalidRewardIndex | Invalid reward index |
| 6027 | 0x178b | RewardVaultAmountInsufficient | Reward vault amount insufficient |
| 6028 | 0x178c | FeeRateMaxExceeded | Fee rate max exceeded |
| 6029 | 0x178d | ProtocolFeeRateMaxExceeded | Protocol fee rate max exceeded |
| 6030 | 0x178e | MultiplicationShiftRightOverflow | Multiplication shift right overflow |
| 6031 | 0x178f | MulDivOverflow | Mul div overflow |
| 6032 | 0x1790 | MulDivInvalidInput | Mul div invalid input |
| 6033 | 0x1791 | MultiplicationOverflow | Multiplication overflow |
| 6034 | 0x1792 | InvalidSqrtPriceLimitDirection | Invalid sqrt price limit direction |
| 6035 | 0x1793 | ZeroTradableAmount | Zero tradable amount |
| 6036 | 0x1794 | AmountOutBelowMinimum | Amount out below minimum |
| 6037 | 0x1795 | AmountInAboveMaximum | Amount in above maximum |
| 6038 | 0x1796 | TickArraySequenceInvalidIndex | Tick array sequence invalid index |
| 6039 | 0x1797 | AmountCalcOverflow | Amount calc overflow |
| 6040 | 0x1798 | AmountRemainingOverflow | Amount remaining overflow |
| 6041 | 0x1799 | InvalidIntermediaryMint | Invalid intermediary mint |
| 6042 | 0x179a | DuplicateTwoHopPool | Duplicate two hop pool |
| 6043 | 0x179b | InvalidBundleIndex | Invalid bundle index |
| 6044 | 0x179c | BundledPositionAlreadyOpened | Bundled position already opened |
| 6045 | 0x179d | BundledPositionAlreadyClosed | Bundled position already closed |
| 6046 | 0x179e | PositionBundleNotDeletable | Position bundle not deletable |
| 6047 | 0x179f | UnsupportedTokenMint | Unsupported token mint |
| 6048 | 0x17a0 | RemainingAccountsInvalidSlice | Remaining accounts invalid slice |
| 6049 | 0x17a1 | RemainingAccountsInsufficient | Remaining accounts insufficient |
| 6050 | 0x17a2 | NoExtraAccountsForTransferHook | No extra accounts for transfer hook |
| 6051 | 0x17a3 | IntermediateTokenAmountMismatch | Intermediate token amount mismatch |
| 6052 | 0x17a4 | TransferFeeCalculationError | Transfer fee calculation error |
| 6053 | 0x17a5 | RemainingAccountsDuplicatedAccountsType | Remaining accounts duplicated accounts type |
| 6054 | 0x17a6 | FullRangeOnlyPool | Full range only pool |
| 6055 | 0x17a7 | TooManySupplementalTickArrays | Too many supplemental tick arrays |
| 6056 | 0x17a8 | DifferentWhirlpoolTickArrayAccount | Different whirlpool tick array account |
| 6057 | 0x17a9 | PartialFillError | Trade resulted in partial fill |
| 6058 | 0x17aa | PositionNotLockable | Position is not lockable |
| 6059 | 0x17ab | OperationNotAllowedOnLockedPosition | Operation not allowed on locked position |
| 6060 | 0x17ac | SameTickRangeNotAllowed | Cannot reset position range with same tick range |
| 6061 | 0x17ad | InvalidAdaptiveFeeConstants | Invalid adaptive fee constants |
| 6062 | 0x17ae | InvalidFeeTierIndex | Invalid fee tier index |
| 6063 | 0x17af | InvalidTradeEnableTimestamp | Invalid trade enable timestamp |
| 6064 | 0x17b0 | TradeNotEnabled | Trade is not enabled yet |

---

## Meteora DLMM Errors (6000+ / 0x1770+)

**Source: [MeteoraAg/dlmm-sdk ts-client/src/dlmm/idl.ts](https://github.com/MeteoraAg/dlmm-sdk/blob/main/ts-client/src/dlmm/idl.ts)**

| Code | Hex | Name | Description |
|------|-----|------|-------------|
| 6000 | 0x1770 | InvalidStartBinIndex | Invalid start bin index |
| 6001 | 0x1771 | InvalidBinId | Invalid bin id |
| 6002 | 0x1772 | InvalidInput | Invalid input data |
| 6003 | 0x1773 | ExceededAmountSlippageTolerance | Exceeded amount slippage tolerance |
| 6004 | 0x1774 | ExceededBinSlippageTolerance | Exceeded bin slippage tolerance |
| 6005 | 0x1775 | CompositionFactorFlawed | Composition factor flawed |
| 6006 | 0x1776 | NonPresetBinStep | Non preset bin step |
| 6007 | 0x1777 | ZeroLiquidity | Zero liquidity |
| 6008 | 0x1778 | InvalidPosition | Invalid position |
| 6009 | 0x1779 | BinArrayNotFound | Bin array not found |
| 6010 | 0x177a | InvalidTokenMint | Invalid token mint |
| 6011 | 0x177b | InvalidAccountForSingleDeposit | Invalid account for single deposit |
| 6012 | 0x177c | PairInsufficientLiquidity | Pair insufficient liquidity |
| 6013 | 0x177d | InvalidFeeOwner | Invalid fee owner |
| 6014 | 0x177e | InvalidFeeWithdrawAmount | Invalid fee withdraw amount |
| 6015 | 0x177f | InvalidAdmin | Invalid admin |
| 6016 | 0x1780 | IdenticalFeeOwner | Identical fee owner |
| 6017 | 0x1781 | InvalidBps | Invalid basis point |
| 6018 | 0x1782 | MathOverflow | Math operation overflow |
| 6019 | 0x1783 | TypeCastFailed | Type cast error |
| 6020 | 0x1784 | InvalidRewardIndex | Invalid reward index |
| 6021 | 0x1785 | InvalidRewardDuration | Invalid reward duration |
| 6022 | 0x1786 | RewardInitialized | Reward already initialized |
| 6023 | 0x1787 | RewardUninitialized | Reward not initialized |
| 6024 | 0x1788 | IdenticalFunder | Identical funder |
| 6025 | 0x1789 | RewardCampaignInProgress | Reward campaign in progress |
| 6026 | 0x178a | IdenticalRewardDuration | Reward duration is the same |
| 6027 | 0x178b | InvalidBinArray | Invalid bin array |
| 6028 | 0x178c | NonContinuousBinArrays | Bin arrays must be continuous |
| 6029 | 0x178d | InvalidRewardVault | Invalid reward vault |
| 6030 | 0x178e | NonEmptyPosition | Position is not empty |
| 6031 | 0x178f | UnauthorizedAccess | Unauthorized access |
| 6032 | 0x1790 | InvalidFeeParameter | Invalid fee parameter |
| 6033 | 0x1791 | MissingOracle | Missing oracle account |
| 6034 | 0x1792 | InsufficientSample | Insufficient observation sample |
| 6035 | 0x1793 | InvalidLookupTimestamp | Invalid lookup timestamp |
| 6036 | 0x1794 | BitmapExtensionAccountIsNotProvided | Bitmap extension account is not provided |
| 6037 | 0x1795 | CannotFindNonZeroLiquidityBinArrayId | Cannot find non-zero liquidity binArrayId |
| 6038 | 0x1796 | BinIdOutOfBound | Bin id out of bound |
| 6039 | 0x1797 | InsufficientOutAmount | Insufficient amount in for minimum out |
| 6040 | 0x1798 | InvalidPositionWidth | Invalid position width |
| 6041 | 0x1799 | ExcessiveFeeUpdate | Excessive fee update |
| 6042 | 0x179a | PoolDisabled | Pool disabled |
| 6043 | 0x179b | InvalidPoolType | Invalid pool type |
| 6044 | 0x179c | ExceedMaxWhitelist | Whitelist for wallet is full |
| 6045 | 0x179d | InvalidIndex | Invalid index |
| 6046 | 0x179e | RewardNotEnded | Reward not ended |
| 6047 | 0x179f | MustWithdrawnIneligibleReward | Must withdraw ineligible reward |
| 6048 | 0x17a0 | UnauthorizedAddress | Unauthorized address |
| 6049 | 0x17a1 | OperatorsAreTheSame | Cannot update because operators are the same |
| 6050 | 0x17a2 | WithdrawToWrongTokenAccount | Withdraw to wrong token account |
| 6051 | 0x17a3 | WrongRentReceiver | Wrong rent receiver |
| 6052 | 0x17a4 | AlreadyPassActivationPoint | Already activated |
| 6053 | 0x17a5 | ExceedMaxSwappedAmount | Swapped amount is exceeded max swapped amount |
| 6054 | 0x17a6 | InvalidStrategyParameters | Invalid strategy parameters |
| 6055 | 0x17a7 | LiquidityLocked | Liquidity locked |
| 6056 | 0x17a8 | BinRangeIsNotEmpty | Bin range is not empty |
| 6057 | 0x17a9 | NotExactAmountOut | Amount out is not matched with exact amount out |
| 6058 | 0x17aa | InvalidActivationType | Invalid activation type |
| 6059 | 0x17ab | InvalidActivationDuration | Invalid activation duration |
| 6060 | 0x17ac | MissingTokenAmountAsTokenLaunchProof | Missing token amount as token launch owner proof |
| 6061 | 0x17ad | InvalidQuoteToken | Quote token must be SOL or USDC |
| 6062 | 0x17ae | InvalidBinStep | Invalid bin step |
| 6063 | 0x17af | InvalidBaseFee | Invalid base fee |
| 6064 | 0x17b0 | InvalidPreActivationDuration | Invalid pre-activation duration |
| 6065 | 0x17b1 | AlreadyPassPreActivationSwapPoint | Already pass pre-activation swap point |
| 6066 | 0x17b2 | InvalidStatus | Invalid status |
| 6067 | 0x17b3 | ExceededMaxOracleLength | Exceed max oracle length |
| 6068 | 0x17b4 | InvalidMinimumLiquidity | Invalid minimum liquidity |
| 6069 | 0x17b5 | NotSupportMint | Not support token_2022 mint extension |
| 6070 | 0x17b6 | UnsupportedMintExtension | Unsupported mint extension |
| 6071 | 0x17b7 | UnsupportNativeMintToken2022 | Unsupported native mint token2022 |
| 6072 | 0x17b8 | UnmatchTokenMint | Unmatch token mint |
| 6073 | 0x17b9 | UnsupportedTokenMint | Unsupported token mint |
| 6074 | 0x17ba | InsufficientRemainingAccounts | Insufficient remaining accounts |
| 6075 | 0x17bb | InvalidRemainingAccountSlice | Invalid remaining account slice |
| 6076 | 0x17bc | DuplicatedRemainingAccountTypes | Duplicated remaining account types |
| 6077 | 0x17bd | MissingRemainingAccountForTransferHook | Missing remaining account for transfer hook |
| 6078 | 0x17be | NoTransferHookProgram | Remaining account was passed for transfer hook but there's no hook program |
| 6079 | 0x17bf | ZeroFundedAmount | Zero funded amount |
| 6080 | 0x17c0 | InvalidSide | Invalid side |
| 6081 | 0x17c1 | InvalidResizeLength | Invalid resize length |
| 6082 | 0x17c2 | NotSupportAtTheMoment | Not support at the moment |
| 6083 | 0x17c3 | InvalidRebalanceParameters | Invalid rebalance parameters |
| 6084 | 0x17c4 | InvalidRewardAccounts | Invalid reward accounts |
| 6085 | 0x17c5 | UndeterminedError | Undetermined error |
| 6086 | 0x17c6 | ReallocExceedMaxLengthPerInstruction | Realloc exceed max length per instruction |
| 6087 | 0x17c7 | InvalidBaseFeeMantissa | Mantissa cannot more than two significant digits |
| 6088 | 0x17c8 | InvalidPositionOwner | Invalid position owner |
| 6089 | 0x17c9 | InvalidPoolAddress | Invalid pool address |

---

## Raydium CLMM Errors (Anchor 6000+ base)

**Source: [raydium-io/raydium-clmm programs/amm/src/error.rs](https://github.com/raydium-io/raydium-clmm/blob/master/programs/amm/src/error.rs)**

| Code | Hex | Name | Description |
|------|-----|------|-------------|
| 6000 | 0x1770 | LOK | Lock error (re-entrancy) |
| 6001 | 0x1771 | NotApproved | Not approved |
| 6002 | 0x1772 | InvalidUpdateConfigFlag | Invalid update amm config flag |
| 6003 | 0x1773 | AccountLack | Account lack |
| 6004 | 0x1774 | ClosePositionErr | Remove liquidity, collect fees owed and reward first |
| 6005 | 0x1775 | ZeroMintAmount | Minting amount should be greater than 0 |
| 6006 | 0x1776 | InvalidTickIndex | Tick out of range |
| 6007 | 0x1777 | TickInvalidOrder | The lower tick must be below the upper tick |
| 6008 | 0x1778 | TickLowerOverflow | The tick must be >= minimum tick(-443636) |
| 6009 | 0x1779 | TickUpperOverflow | The tick must be <= maximum tick(443636) |
| 6010 | 0x177a | TickAndSpacingNotMatch | tick % tick_spacing must be zero |
| 6011 | 0x177b | InvalidTickArray | Invalid tick array account |
| 6012 | 0x177c | InvalidTickArrayBoundary | Invalid tick array boundary |
| 6013 | 0x177d | SqrtPriceLimitOverflow | Square root price limit overflow |
| 6014 | 0x177e | SqrtPriceX64 | sqrt_price_x64 out of range |
| 6015 | 0x177f | LiquiditySubValueErr | Liquidity sub delta L must be smaller than before |
| 6016 | 0x1780 | LiquidityAddValueErr | Liquidity add delta L must be >= before |
| 6017 | 0x1781 | InvalidLiquidity | Invalid liquidity when update position |
| 6018 | 0x1782 | ForbidBothZeroForSupplyLiquidity | Both token amount must not be zero while supply liquidity |
| 6019 | 0x1783 | LiquidityInsufficient | Liquidity insufficient |
| 6020 | 0x1784 | TransactionTooOld | Transaction too old |
| 6021 | 0x1785 | PriceSlippageCheck | Price slippage check |
| 6022 | 0x1786 | TooLittleOutputReceived | Too little output received |
| 6023 | 0x1787 | TooMuchInputPaid | Too much input paid |
| 6024 | 0x1788 | ZeroAmountSpecified | Swap special amount can not be zero |
| 6025 | 0x1789 | InvalidInputPoolVault | Input pool vault is invalid |
| 6026 | 0x178a | TooSmallInputOrOutputAmount | Swap input or output amount is too small |
| 6027 | 0x178b | NotEnoughTickArrayAccount | Not enough tick array account |
| 6028 | 0x178c | InvalidFirstTickArrayAccount | Invalid first tick array account |
| 6029 | 0x178d | InvalidRewardIndex | Invalid reward index |
| 6030 | 0x178e | FullRewardInfo | The init reward token reach to the max |
| 6031 | 0x178f | RewardTokenAlreadyInUse | The init reward token already in use |
| 6032 | 0x1790 | ExceptRewardMint | The reward tokens must contain one of pool vault mint except the last reward |
| 6033 | 0x1791 | InvalidRewardInitParam | Invalid reward init param |
| 6034 | 0x1792 | InvalidRewardDesiredAmount | Invalid collect reward desired amount |
| 6035 | 0x1793 | InvalidRewardInputAccountNumber | Invalid collect reward input account number |
| 6036 | 0x1794 | InvalidRewardPeriod | Invalid reward period |
| 6037 | 0x1795 | NotApproveUpdateRewardEmissiones | Modification of emissiones is allowed within 72 hours from the end of the previous cycle |
| 6038 | 0x1796 | UnInitializedRewardInfo | Uninitialized reward info |
| 6039 | 0x1797 | NotSupportMint | Not support token_2022 mint extension |
| 6040 | 0x1798 | MissingTickArrayBitmapExtensionAccount | Missing tickarray bitmap extension account |
| 6041 | 0x1799 | InsufficientLiquidityForDirection | Insufficient liquidity for this direction |
| 6042 | 0x179a | MaxTokenOverflow | Max token overflow |
| 6043 | 0x179b | CalculateOverflow | Calculate overflow |
| 6044 | 0x179c | TransferFeeCalculateNotMatch | TransferFee calculate not match |

---

## Raydium AMM V4 Errors (Non-Anchor, 0-based)

**Source: [raydium-io/raydium-amm program/src/error.rs](https://github.com/raydium-io/raydium-amm/blob/master/program/src/error.rs)**
**NOTE: These are native ProgramError codes (0-based), NOT Anchor #[error_code] (6000-based)**

| Code | Hex | Name | Description |
|------|-----|------|-------------|
| 0 | 0x0 | AlreadyInUse | Already in use |
| 1 | 0x1 | InvalidProgramAddress | Invalid program address |
| 2 | 0x2 | ExpectedMint | Expected mint |
| 3 | 0x3 | ExpectedAccount | Expected account |
| 4 | 0x4 | InvalidCoinVault | Invalid coin vault |
| 5 | 0x5 | InvalidPCVault | Invalid PC vault |
| 6 | 0x6 | InvalidTokenLP | Invalid token LP |
| 7 | 0x7 | InvalidDestTokenCoin | Invalid dest token coin |
| 8 | 0x8 | InvalidDestTokenPC | Invalid dest token PC |
| 9 | 0x9 | InvalidPoolMint | Invalid pool mint |
| 10 | 0xa | InvalidOpenOrders | Invalid open orders |
| 11 | 0xb | InvalidSerumMarket | Invalid serum market |
| 12 | 0xc | InvalidSerumProgram | Invalid serum program |
| 13 | 0xd | InvalidTargetOrders | Invalid target orders |
| 14 | 0xe | InvalidWithdrawQueue | Invalid withdraw queue |
| 15 | 0xf | InvalidTempLp | Invalid temp LP |
| 16 | 0x10 | InvalidCoinMint | Invalid coin mint |
| 17 | 0x11 | InvalidPCMint | Invalid PC mint |
| 18 | 0x12 | InvalidOwner | Invalid owner |
| 19 | 0x13 | InvalidSupply | Invalid supply |
| 20 | 0x14 | InvalidDelegate | Invalid delegate |
| 21 | 0x15 | InvalidSignAccount | Invalid sign account |
| 22 | 0x16 | InvalidStatus | Invalid status |
| 23 | 0x17 | InvalidInstruction | Invalid instruction |
| 24 | 0x18 | WrongAccountsNumber | Wrong accounts number |
| 25 | 0x19 | WithdrawTransferBusy | Withdraw transfer busy |
| 26 | 0x1a | WithdrawQueueFull | Withdraw queue full |
| 27 | 0x1b | WithdrawQueueEmpty | Withdraw queue empty |
| 28 | 0x1c | CoinSwapAmtLessThanFees | Coin swap amount less than fees |
| 29 | 0x1d | PCSwapAmtLessThanFees | PC swap amount less than fees |
| 30 | 0x1e | DestOrdersIsLessThanBigest | Dest orders less than biggest |
| 31 | 0x1f | CalculationExRateFailure | Calculation exchange rate failure |
| 32 | 0x20 | InvalidStatusToSellPc | Invalid status to sell PC |
| 33 | 0x21 | InvalidStatusToSellCoin | Invalid status to sell coin |
| 34 | 0x22 | InvalidStatusToDeposit | Invalid status to deposit |
| 35 | 0x23 | InvalidStatusToWithdraw | Invalid status to withdraw |
| 36 | 0x24 | InvalidStatusToSwap | Invalid status to swap |
| 37 | 0x25 | InvalidStateData | Invalid state data |
| 38 | 0x26 | InvalidInput | Invalid input |
| 39 | 0x27 | ExceededSlippage | Exceeded slippage |
| 40 | 0x28 | InsufficientFunds | Insufficient funds |
| 41 | 0x29 | ConversionFailure | Conversion failure |
| 42 | 0x2a | InvalidUserToken | Invalid user token |
| 43 | 0x2b | InvalidSrmMint | Invalid SRM mint |
| 44 | 0x2c | InvalidSrmToken | Invalid SRM token |
| 45 | 0x2d | TooManyOpenOrders | Too many open orders |
| 46 | 0x2e | OrderAtSlotIsPlaced | Order at slot is placed |
| 47 | 0x2f | InvalidSysProgramAddress | Invalid system program address |
| 48 | 0x30 | InvalidFee | Invalid fee |
| 49 | 0x31 | RepeatCreateAmm | Repeat create AMM |
| 50 | 0x32 | NotAllowZeroLP | Not allow zero LP |
| 51 | 0x33 | InvalidCloseAuthority | Invalid close authority |
| 52 | 0x34 | InvalidFreezeAuthority | Invalid freeze authority |
| 53 | 0x35 | InvalidReferPCMint | Invalid refer PC mint |
| 54 | 0x36 | InvalidConfigAccount | Invalid config account |

---

## Pancake CLMM Errors (6000+ / 0x1770+)

Program ID: `HpNfyc2Saw7RKkQd8nEL4khUcuPhQ7WwY1B2qjx8jxFq`

| Code | Hex | Name | Description |
|------|-----|------|-------------|
| 6000 | 0x1770 | LOK | LOK (re-entrancy lock) |
| 6001 | 0x1771 | NotApproved | Not approved |
| 6002 | 0x1772 | InvalidUpdateConfigFlag | Invalid update amm config flag |
| 6003 | 0x1773 | AccountLack | Account lack |
| 6004 | 0x1774 | ClosePositionErr | Remove liquidity, collect fees owed and reward then you can close position account |
| 6005 | 0x1775 | ZeroMintAmount | Minting amount should be greater than 0 |
| 6006 | 0x1776 | InvalidTickIndex | Tick out of range |
| 6007 | 0x1777 | TickInvalidOrder | The lower tick must be below the upper tick |
| 6008 | 0x1778 | TickLowerOverflow | The tick must be greater, or equal to the minimum tick(-443636) |
| 6009 | 0x1779 | TickUpperOverflow | The tick must be lesser than, or equal to the maximum tick(443636) |
| 6010 | 0x177a | TickAndSpacingNotMatch | tick % tick_spacing must be zero |
| 6011 | 0x177b | InvalidTickArray | Invalid tick array account |
| 6012 | 0x177c | InvalidTickArrayBoundary | Invalid tick array boundary |
| 6013 | 0x177d | SqrtPriceLimitOverflow | Square root price limit overflow |
| 6014 | 0x177e | SqrtPriceX64 | sqrt_price_x64 out of range |
| 6015 | 0x177f | LiquiditySubValueErr | Liquidity sub delta L must be smaller than before |
| 6016 | 0x1780 | LiquidityAddValueErr | Liquidity add delta L must be greater, or equal to before |
| 6017 | 0x1781 | InvalidLiquidity | Invalid liquidity when update position |
| 6018 | 0x1782 | ForbidBothZeroForSupplyLiquidity | Both token amount must not be zero while supply liquidity |
| 6019 | 0x1783 | LiquidityInsufficient | Liquidity insufficient |
| 6020 | 0x1784 | TransactionTooOld | Transaction too old |
| 6021 | 0x1785 | PriceSlippageCheck | Price slippage check |
| 6022 | 0x1786 | TooLittleOutputReceived | Too little output received |
| 6023 | 0x1787 | TooMuchInputPaid | Too much input paid |
| 6024 | 0x1788 | ZeroAmountSpecified | Swap special amount can not be zero |
| 6025 | 0x1789 | InvalidInputPoolVault | Input pool vault is invalid |
| 6026 | 0x178a | TooSmallInputOrOutputAmount | Swap input or output amount is too small |
| 6027 | 0x178b | NotEnoughTickArrayAccount | Not enough tick array account |
| 6028 | 0x178c | InvalidFirstTickArrayAccount | Invalid first tick array account |
| 6029 | 0x178d | InvalidRewardIndex | Invalid reward index |
| 6030 | 0x178e | FullRewardInfo | The init reward token reach to the max |
| 6031 | 0x178f | RewardTokenAlreadyInUse | The init reward token already in use |
| 6032 | 0x1790 | ExceptRewardMint | The reward tokens must contain one of pool vault mint except the last reward |
| 6033 | 0x1791 | InvalidRewardInitParam | Invalid reward init param |
| 6034 | 0x1792 | InvalidRewardDesiredAmount | Invalid collect reward desired amount |
| 6035 | 0x1793 | InvalidRewardInputAccountNumber | Invalid collect reward input account number |
| 6036 | 0x1794 | InvalidRewardPeriod | Invalid reward period |
| 6037 | 0x1795 | NotApproveUpdateRewardEmissiones | Modification of emissiones is allowed within 72 hours from the end of the previous cycle |
| 6038 | 0x1796 | UnInitializedRewardInfo | Uninitialized reward info |
| 6039 | 0x1797 | NotSupportMint | Not support token_2022 mint extension |
| 6040 | 0x1798 | MissingTickArrayBitmapExtensionAccount | Missing tickarray bitmap extension account |
| 6041 | 0x1799 | InsufficientLiquidityForDirection | Insufficient liquidity for this direction |
| 6042 | 0x179a | MaxTokenOverflow | Max token overflow |
| 6043 | 0x179b | CalculateOverflow | Calculate overflow |
| 6044 | 0x179c | TransferFeeCalculateNotMatch | TransferFee calculate not match |

---

## Meteora DAMM v2 / CP Errors (6000+ / 0x1770+)

Program ID: `cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG`

**Source: [MeteoraAg/damm-v2 programs/cp-amm/src/error.rs](https://github.com/MeteoraAg/damm-v2/blob/main/programs/cp-amm/src/error.rs)**

| Code | Hex | Name | Description |
|------|-----|------|-------------|
| 6000 | 0x1770 | MathOverflow | Math operation overflow |
| 6001 | 0x1771 | InvalidFee | Invalid fee setup |
| 6002 | 0x1772 | **ExceededSlippage** | **Exceeded slippage tolerance** |
| 6003 | 0x1773 | PoolDisabled | Pool disabled |
| 6004 | 0x1774 | ExceedMaxFeeBps | Exceeded max fee bps |
| 6005 | 0x1775 | InvalidAdmin | Invalid admin |
| 6006 | 0x1776 | AmountIsZero | Amount is zero |
| 6007 | 0x1777 | TypeCastFailed | Type cast error |
| 6008 | 0x1778 | UnableToModifyActivationPoint | Unable to modify activation point |
| 6009 | 0x1779 | InvalidAuthorityToCreateThePool | Invalid authority to create the pool |
| 6010 | 0x177a | InvalidActivationType | Invalid activation type |
| 6011 | 0x177b | InvalidActivationPoint | Invalid activation point |
| 6012 | 0x177c | InvalidQuoteMint | Quote token must be SOL,USDC |
| 6013 | 0x177d | InvalidFeeCurve | Invalid fee curve |
| 6014 | 0x177e | InvalidPriceRange | Invalid price range |
| 6015 | 0x177f | PriceRangeViolation | Trade is over price range |
| 6016 | 0x1780 | InvalidParameters | Invalid parameters |
| 6017 | 0x1781 | InvalidCollectFeeMode | Invalid collect fee mode |
| 6018 | 0x1782 | InvalidInput | Invalid input |
| 6019 | 0x1783 | CannotCreateTokenBadgeOnSupportedMint | Cannot create token badge on supported mint |
| 6020 | 0x1784 | InvalidTokenBadge | Invalid token badge |
| 6021 | 0x1785 | InvalidMinimumLiquidity | Invalid minimum liquidity |
| 6022 | 0x1786 | InvalidVestingInfo | Invalid vesting information |
| 6023 | 0x1787 | InsufficientLiquidity | Insufficient liquidity |
| 6024 | 0x1788 | InvalidVestingAccount | Invalid vesting account |
| 6025 | 0x1789 | InvalidPoolStatus | Invalid pool status |
| 6026 | 0x178a | UnsupportNativeMintToken2022 | Unsupported native mint token2022 |
| 6027 | 0x178b | InvalidRewardIndex | Invalid reward index |
| 6028 | 0x178c | InvalidRewardDuration | Invalid reward duration |
| 6029 | 0x178d | RewardInitialized | Reward already initialized |
| 6030 | 0x178e | RewardUninitialized | Reward not initialized |
| 6031 | 0x178f | InvalidRewardVault | Invalid reward vault |
| 6032 | 0x1790 | MustWithdrawnIneligibleReward | Must withdraw ineligible reward |
| 6033 | 0x1791 | IdenticalRewardDuration | Reward duration is the same |
| 6034 | 0x1792 | RewardCampaignInProgress | Reward campaign in progress |
| 6035 | 0x1793 | IdenticalFunder | Identical funder |
| 6036 | 0x1794 | InvalidFunder | Invalid funder |
| 6037 | 0x1795 | RewardNotEnded | Reward not ended |
| 6038 | 0x1796 | FeeInverseIsIncorrect | Fee inverse is incorrect |
| 6039 | 0x1797 | PositionIsNotEmpty | Position is not empty |
| 6040 | 0x1798 | InvalidPoolCreatorAuthority | Invalid pool creator authority |
| 6041 | 0x1799 | InvalidConfigType | Invalid config type |
| 6042 | 0x179a | InvalidPoolCreator | Invalid pool creator |
| 6043 | 0x179b | RewardVaultFrozenSkipRequired | Reward vault is frozen, must skip reward to proceed |
| 6044 | 0x179c | InvalidSplitPositionParameters | Invalid parameters for split position |
| 6045 | 0x179d | UnsupportPositionHasVestingLock | Unsupported split position has vesting lock |
| 6046 | 0x179e | SamePosition | Same position |
| 6047 | 0x179f | InvalidBaseFeeMode | Invalid base fee mode |
| 6048 | 0x17a0 | InvalidFeeRateLimiter | Invalid fee rate limiter |
| 6049 | 0x17a1 | FailToValidateSingleSwapInstruction | Fail to validate single swap instruction in rate limiter |
| 6050 | 0x17a2 | InvalidFeeScheduler | Invalid fee scheduler |
| 6051 | 0x17a3 | UndeterminedError | Undetermined error |
| 6052 | 0x17a4 | InvalidPoolVersion | Invalid pool version |

---

## Solend Errors (Non-Anchor, 0-based)

Program ID: `So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo`

**Source:** [solendprotocol/solana-program-library - sdk/src/error.rs](https://github.com/solendprotocol/solana-program-library/blob/main/token-lending/sdk/src/error.rs)

| Code | Hex | Name | Description |
|------|-----|------|-------------|
| 0 | 0x0 | InstructionUnpackError | Failed to unpack instruction data |
| 1 | 0x1 | AlreadyInitialized | Account is already initialized |
| 2 | 0x2 | NotRentExempt | Lamport balance below rent-exempt threshold |
| 3 | 0x3 | InvalidMarketAuthority | Market authority is invalid |
| 4 | 0x4 | InvalidMarketOwner | Market owner is invalid |
| 5 | 0x5 | InvalidAccountOwner | Input account owner is not the program address |
| 6 | 0x6 | InvalidTokenOwner | Input token account is not owned by the correct token program id |
| 7 | 0x7 | InvalidTokenAccount | Input token account is not valid |
| 8 | 0x8 | InvalidTokenMint | Input token mint account is not valid |
| 9 | 0x9 | InvalidTokenProgram | Input token program account is not valid |
| 10 | 0xa | InvalidAmount | Input amount is invalid |
| 11 | 0xb | InvalidConfig | Input config value is invalid |
| 12 | 0xc | InvalidSigner | Input account must be a signer |
| 13 | 0xd | InvalidAccountInput | Invalid account input |
| 14 | 0xe | MathOverflow | Math operation overflow |
| 15 | 0xf | TokenInitializeMintFailed | Token initialize mint failed |
| 16 | 0x10 | TokenInitializeAccountFailed | Token initialize account failed |
| 17 | 0x11 | TokenTransferFailed | Token transfer failed |
| 18 | 0x12 | TokenMintToFailed | Token mint to failed |
| 19 | 0x13 | TokenBurnFailed | Token burn failed |
| 20 | 0x14 | InsufficientLiquidity | Insufficient liquidity available |
| 21 | 0x15 | ReserveCollateralDisabled | Input reserve has collateral disabled |
| 22 | 0x16 | ReserveStale | Reserve state needs to be refreshed |
| 23 | 0x17 | WithdrawTooSmall | Withdraw amount too small |
| 24 | 0x18 | WithdrawTooLarge | Withdraw amount too large |
| 25 | 0x19 | BorrowTooSmall | Borrow amount too small to receive liquidity after fees |
| 26 | 0x1a | BorrowTooLarge | Borrow amount too large for deposited collateral |
| 27 | 0x1b | RepayTooSmall | Repay amount too small to transfer liquidity |
| 28 | 0x1c | LiquidationTooSmall | Liquidation amount too small to receive collateral |
| 29 | 0x1d | ObligationHealthy | Cannot liquidate healthy obligations |
| 30 | 0x1e | ObligationStale | Obligation state needs to be refreshed |
| 31 | 0x1f | ObligationReserveLimit | Obligation reserve limit exceeded |
| 32 | 0x20 | InvalidObligationOwner | Obligation owner is invalid |
| 33 | 0x21 | ObligationDepositsEmpty | Obligation deposits are empty |
| 34 | 0x22 | ObligationBorrowsEmpty | Obligation borrows are empty |
| 35 | 0x23 | ObligationDepositsZero | Obligation deposits have zero value |
| 36 | 0x24 | ObligationBorrowsZero | Obligation borrows have zero value |
| 37 | 0x25 | InvalidObligationCollateral | Invalid obligation collateral |
| 38 | 0x26 | InvalidObligationLiquidity | Invalid obligation liquidity |
| 39 | 0x27 | ObligationCollateralEmpty | Obligation collateral is empty |
| 40 | 0x28 | ObligationLiquidityEmpty | Obligation liquidity is empty |
| 41 | 0x29 | NegativeInterestRate | Interest rate is negative |
| 42 | 0x2a | InvalidOracleConfig | Input oracle config is invalid |
| 43 | 0x2b | InvalidFlashLoanReceiverProgram | Input flash loan receiver program account is not valid |
| 44 | 0x2c | NotEnoughLiquidityAfterFlashLoan | Not enough liquidity after flash loan |
| 45 | 0x2d | NullOracleConfig | Null oracle config |
| 46 | 0x2e | InsufficientProtocolFeesToRedeem | Insufficent protocol fees to claim or no liquidity available |
| 47 | 0x2f | FlashBorrowCpi | No cpi flash borrows allowed |
| 48 | 0x30 | NoFlashRepayFound | No corresponding repay found for flash borrow |
| 49 | 0x31 | InvalidFlashRepay | Invalid repay found |
| 50 | 0x32 | FlashRepayCpi | No cpi flash repays allowed |
| 51 | 0x33 | MultipleFlashBorrows | Multiple flash borrows not allowed in the same transaction |
| 52 | 0x34 | FlashLoansDisabled | Flash loans are disabled for this reserve |
| 53 | 0x35 | DeprecatedInstruction | Instruction is deprecated |
| 54 | 0x36 | OutflowRateLimitExceeded | Outflow rate limit exceeded |
| 55 | 0x37 | NotWhitelistedLiquidator | Not a whitelisted liquidator |
| 56 | 0x38 | IsolatedTierAssetViolation | Isolated tier asset violation |
| 57 | 0x39 | BorrowAttributionLimitExceeded | Borrow attribution limit exceeded |
| 58 | 0x3a | BorrowAttributionLimitNotExceeded | Borrow attribution limit not exceeded |

---

## Token Program / Token-2022 Errors (0-68)

Program IDs: 
- Token Program: `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`
- Token-2022: `TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb`

**Source:** [solana-program/token-2022 - interface/src/error.rs](https://github.com/solana-program/token-2022/blob/main/interface/src/error.rs)

**Note:** Token-2022 uses the SAME error codes 0-19 as legacy Token Program, plus additional codes 20-68.

| Code | Hex | Name | Description |
|------|-----|------|-------------|
| 0 | 0x0 | NotRentExempt | Lamport balance below rent-exempt threshold |
| 1 | 0x1 | InsufficientFunds | Insufficient funds for the operation requested |
| 2 | 0x2 | InvalidMint | Invalid Mint |
| 3 | 0x3 | MintMismatch | Account not associated with this Mint |
| 4 | 0x4 | OwnerMismatch | Owner does not match |
| 5 | 0x5 | FixedSupply | This token's supply is fixed and new tokens cannot be minted |
| 6 | 0x6 | AlreadyInUse | Account cannot be initialized because it is already being used |
| 7 | 0x7 | InvalidNumberOfProvidedSigners | Invalid number of provided signers |
| 8 | 0x8 | InvalidNumberOfRequiredSigners | Invalid number of required signers |
| 9 | 0x9 | UninitializedState | State is uninitialized |
| 10 | 0xa | NativeNotSupported | Instruction does not support native tokens |
| 11 | 0xb | NonNativeHasBalance | Non-native account can only be closed if its balance is zero |
| 12 | 0xc | InvalidInstruction | Invalid instruction |
| 13 | 0xd | InvalidState | State is invalid for requested operation |
| 14 | 0xe | Overflow | Operation overflowed |
| 15 | 0xf | AuthorityTypeNotSupported | Account does not support specified authority type |
| 16 | 0x10 | MintCannotFreeze | This token mint cannot freeze accounts |
| 17 | 0x11 | AccountFrozen | Account is frozen |
| 18 | 0x12 | MintDecimalsMismatch | The provided decimals value different from the Mint decimals |
| 19 | 0x13 | NonNativeNotSupported | Instruction does not support non-native tokens |
| 20 | 0x14 | ExtensionTypeMismatch | Extension type does not match already existing extensions |
| 21 | 0x15 | ExtensionBaseMismatch | Extension does not match the base type provided |
| 22 | 0x16 | ExtensionAlreadyInitialized | Extension already initialized on this account |
| 23 | 0x17 | ConfidentialTransferAccountHasBalance | Account can only be closed if its confidential balance is zero |
| 24 | 0x18 | ConfidentialTransferAccountNotApproved | Account not approved for confidential transfers |
| 25 | 0x19 | ConfidentialTransferDepositsAndTransfersDisabled | Account not accepting deposits or transfers |
| 26 | 0x1a | ConfidentialTransferElGamalPubkeyMismatch | ElGamal public key mismatch |
| 27 | 0x1b | ConfidentialTransferBalanceMismatch | Balance mismatch |
| 28 | 0x1c | MintHasSupply | Mint has non-zero supply. Burn all tokens before closing the mint |
| 29 | 0x1d | NoAuthorityExists | No authority exists to perform the desired operation |
| 30 | 0x1e | TransferFeeExceedsMaximum | Transfer fee exceeds maximum of 10,000 basis points |
| 31 | 0x1f | MintRequiredForTransfer | Mint required for this account to transfer tokens |
| 32 | 0x20 | FeeMismatch | Calculated fee does not match expected fee |
| 33 | 0x21 | FeeParametersMismatch | Fee parameters in ZK proofs do not match fee parameters in mint |
| 34 | 0x22 | ImmutableOwner | The owner authority cannot be changed |
| 35 | 0x23 | AccountHasWithheldTransferFees | Account can only be closed if withheld fee balance is zero |
| 36 | 0x24 | NoMemo | No memo in previous instruction; required for recipient to receive transfer |
| 37 | 0x25 | NonTransferable | Transfer is disabled for this mint |
| 38 | 0x26 | NonTransferableNeedsImmutableOwnership | Non-transferable tokens can't be minted to account without immutable ownership |
| 39 | 0x27 | MaximumPendingBalanceCreditCounterExceeded | Total Deposit/Transfer instructions exceed maximum_pending_balance_credit_counter |
| 40 | 0x28 | MaximumDepositAmountExceeded | Deposit amount exceeds maximum limit |
| 41 | 0x29 | CpiGuardSettingsLocked | CPI Guard cannot be enabled or disabled in CPI |
| 42 | 0x2a | CpiGuardTransferBlocked | CPI Guard: program attempted to transfer user funds without using a delegate |
| 43 | 0x2b | CpiGuardBurnBlocked | CPI Guard: program attempted to burn user funds without using a delegate |
| 44 | 0x2c | CpiGuardCloseAccountBlocked | CPI Guard: program attempted to close account without returning lamports to owner |
| 45 | 0x2d | CpiGuardApproveBlocked | CPI Guard: program attempted to approve a delegate |
| 46 | 0x2e | CpiGuardSetAuthorityBlocked | CPI Guard: program attempted to add or change an authority |
| 47 | 0x2f | CpiGuardOwnerChangeBlocked | Account ownership cannot be changed while CPI Guard is enabled |
| 48 | 0x30 | ExtensionNotFound | Extension not found in account data |
| 49 | 0x31 | NonConfidentialTransfersDisabled | Non-confidential transfers disabled |
| 50 | 0x32 | ConfidentialTransferFeeAccountHasWithheldFee | Account has non-zero confidential withheld fee |
| 51 | 0x33 | InvalidExtensionCombination | Mint or account initialized to invalid combination of extensions |
| 52 | 0x34 | InvalidLengthForAlloc | Extension allocation with overwrite must use the same length |
| 53 | 0x35 | AccountDecryption | Failed to decrypt a confidential transfer account |
| 54 | 0x36 | ProofGeneration | Failed to generate proof |
| 55 | 0x37 | InvalidProofInstructionOffset | An invalid proof instruction offset was provided |
| 56 | 0x38 | HarvestToMintDisabled | Harvest of withheld tokens to mint is disabled |
| 57 | 0x39 | SplitProofContextStateAccountsNotSupported | Split proof context state accounts not supported for instruction |
| 58 | 0x3a | NotEnoughProofContextStateAccounts | Not enough proof context state accounts provided |
| 59 | 0x3b | MalformedCiphertext | Ciphertext is malformed |
| 60 | 0x3c | CiphertextArithmeticFailed | Ciphertext arithmetic failed |
| 61 | 0x3d | PedersenCommitmentMismatch | Pedersen commitments did not match |
| 62 | 0x3e | RangeProofLengthMismatch | Range proof length did not match |
| 63 | 0x3f | IllegalBitLength | Illegal transfer amount bit length |
| 64 | 0x40 | FeeCalculation | Fee calculation failed |
| 65 | 0x41 | IllegalMintBurnConversion | Withdraw/Deposit not allowed for confidential-mint-burn |
| 66 | 0x42 | InvalidScale | Invalid scale for scaled ui amount |
| 67 | 0x43 | MintPaused | Transferring, minting, and burning is paused on this mint |
| 68 | 0x44 | PendingBalanceNonZero | Key rotation attempted while pending balance is not zero |

---

## Quick Lookup: Common Swap Errors

| Hex | Most Likely Meaning | Action Required |
|-----|---------------------|-----------------|
| 0x1794 | Slippage - output below minimum | Increase slippage tolerance or reduce trade size |
| 0x1795 | Slippage - input above maximum | Increase slippage tolerance or reduce trade size |
| 0x1797 | Insufficient output / Amount calc overflow | Check liquidity estimation |
| 0x177c | Insufficient liquidity in pool | Skip this pool or reduce trade size |
| 0x177d | Liquidity too high / Slippage check | Check price impact calculation |
| 0x1785 | Calculate overflow | Fix math calculations, use checked arithmetic |
| 0x1786 | Insufficient liquidity for direction | Swap direction wrong or no liquidity |
| 0x28 | Raydium V4 insufficient funds | Check token balance |
| 0x27 | Raydium V4 exceeded slippage | Increase slippage tolerance |
| 0x7d3 | Anchor constraint raw failed | Check custom validation logic |
| 0xbbd | Not enough accounts provided | Missing accounts in transaction |

---

## How to Determine Error Source

Parse the simulation failure message to find the program ID:

```
Program <PROGRAM_ID> failed: custom program error: 0x1772
```

Then match the program ID to know which error table to use.

---

## Token-2022 Support Matrix

Token-2022 (SPL Token Extensions) is the new token standard. **Many new tokens use Token-2022.**

### Protocol Support Status

| Protocol | Token-2022 Support | Notes |
|----------|-------------------|-------|
| **Raydium AMM V4** | ❌ NO | Hardcoded check: `token_program_info.key == spl_token::id()` - Causes `0x26 InvalidSplTokenProgram` |
| **Raydium CLMM** | ✅ YES | SwapV2 has separate `token_program` and `token_program_2022` accounts |
| **Orca Whirlpool V2** | ✅ YES | SwapV2 has `token_program_a` and `token_program_b` per token |
| **Meteora DLMM** | ✅ YES | Has `token_x_program` and `token_y_program` accounts |
| **Meteora DAMM v2** | ✅ YES | Uses `TokenInterface` - handles both automatically |
| **Meteora CP** | ⚠️ PARTIAL | Some pools support it, check pool state |

### Why 0x26 `InvalidSplTokenProgram` Happens

Raydium V4's `process.rs` has this check in `unpack_token_account()`:

```rust
fn unpack_token_account(account_info: &AccountInfo, token_program_id: &Pubkey) -> Result<...> {
    if account_info.owner != token_program_id {
        Err(AmmError::InvalidSplTokenProgram.into())  // 0x26 (38)
    }
}
```

When you pass a Token-2022 token account (owned by `TokenzQdBNbLqP...`) but the instruction
expects the Token Program (`TokenkegQfeZyi...`), this check fails.

### Solution for Token-2022

1. **Detect the token program from mint account owner:**
   ```rust
   pub fn from_mint_owner(owner: &Pubkey) -> Option<TokenProgramType> {
       if *owner == TOKEN_PROGRAM_ID {
           Some(TokenProgramType::Token)
       } else if *owner == TOKEN_2022_PROGRAM_ID {
           Some(TokenProgramType::Token2022)
       } else {
           None
       }
   }
   ```

2. **Use correct ATA derivation:**
   ```rust
   // Token and Token-2022 ATAs are DIFFERENT because token_program is in the seed
   pub fn derive_ata_with_program(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
       Pubkey::find_program_address(
           &[owner.as_ref(), token_program.as_ref(), mint.as_ref()],
           &ATA_PROGRAM_ID,
       ).0
   }
   ```

3. **For protocols with dual token program support (CLMM, Whirlpool V2, DLMM):**
   - Pass the correct token program in the appropriate account slot
   - Orca: `token_program_a` / `token_program_b`
   - Raydium CLMM: `token_program` / `token_program_2022`
   - Meteora: `token_x_program` / `token_y_program`

4. **For Raydium V4 specifically:**
   - This protocol CANNOT support Token-2022 tokens
   - Must filter out opportunities involving Token-2022 tokens on Raydium V4
   - OR use Raydium CLMM (which supports Token-2022) for those pairs instead

### Key Constants

```rust
pub const TOKEN_PROGRAM_ID: Pubkey = 
    pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

pub const TOKEN_2022_PROGRAM_ID: Pubkey = 
    pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
```

---