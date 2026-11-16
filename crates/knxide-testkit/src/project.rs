//! A made-up project for tests: three application programs, one per mask the planner supports,
//! and a project file with devices that use them, plus devices that cannot be downloaded.
//!
//! No manufacturer data is involved; every identifier, address and value is invented.

use crate::zip;

/// Application program for mask 0021 (BCU 2): dynamic tables, load control through
/// properties, memory writes echoed in verify mode.
///
/// Segments: 0100 (memory header, masked), 0116 (address table, 6 entries), 0126 (association
/// table, 5 entries), 0200 (object table of 3 objects, then parameters), 0BB8 (RAM, no data).
pub const BCU2_PRODUCT: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<KNX xmlns="http://knx.org/xml/project/20"><ManufacturerData><Manufacturer RefId="M-00FA">
<ApplicationPrograms>
<ApplicationProgram Id="M-00FA_A-0102-03-0000" ApplicationNumber="258" ApplicationVersion="3"
    MaskVersion="MV-0021" LoadProcedureStyle="ProductProcedure" PeiType="0"
    DynamicTableManagement="true">
  <Static>
    <Code>
      <AbsoluteSegment Id="M-00FA_A-0102-03-0000_AS-0116" Address="278" Size="16">
        <Data>AQAA/////////////////w==</Data><Mask>/wAA////////////////AA==</Mask>
      </AbsoluteSegment>
      <AbsoluteSegment Id="M-00FA_A-0102-03-0000_AS-0126" Address="294" Size="12">
        <Data>Bf//////////////</Data><Mask>//////////////8A</Mask>
      </AbsoluteSegment>
      <AbsoluteSegment Id="M-00FA_A-0102-03-0000_AS-0100" Address="256" Size="22">
        <Data>qgAAAAAAAAAAAAAAAAARIjMAAAAAAA==</Data><Mask>/wAAAAAAAAAAAAAAAAD///8AAAAAAA==</Mask>
      </AbsoluteSegment>
      <AbsoluteSegment Id="M-00FA_A-0102-03-0000_AS-0BB8" Address="3000" Size="10" />
      <AbsoluteSegment Id="M-00FA_A-0102-03-0000_AS-0200" Address="512" Size="16">
        <Data>A+DhNwDid0fjFwMFD/8SNA==</Data><Mask>//////////////////8AAA==</Mask>
      </AbsoluteSegment>
    </Code>
    <ParameterTypes>
      <ParameterType Id="M-00FA_A-0102-03-0000_PT-U8"><TypeNumber SizeInBit="8" Type="unsignedInt" minInclusive="0" maxInclusive="255" /></ParameterType>
      <ParameterType Id="M-00FA_A-0102-03-0000_PT-Nibble"><TypeRestriction Base="Value" SizeInBit="4">
        <Enumeration Text="three" Value="3" Id="M-00FA_A-0102-03-0000_PT-Nibble_EN-3" />
        <Enumeration Text="nine" Value="9" Id="M-00FA_A-0102-03-0000_PT-Nibble_EN-9" />
      </TypeRestriction></ParameterType>
      <ParameterType Id="M-00FA_A-0102-03-0000_PT-Page"><TypeNone /></ParameterType>
      <ParameterType Id="M-00FA_A-0102-03-0000_PT-Mode"><TypeRestriction Base="Value" SizeInBit="8">
        <Enumeration Text="Off" Value="0" DisplayOrder="0" Id="M-00FA_A-0102-03-0000_PT-Mode_EN-0" />
        <Enumeration Text="Switching" Value="1" DisplayOrder="1" Id="M-00FA_A-0102-03-0000_PT-Mode_EN-1" />
        <Enumeration Text="Dimming" Value="2" DisplayOrder="2" Id="M-00FA_A-0102-03-0000_PT-Mode_EN-2" />
        <Enumeration Text="Dimming with scenes" Value="3" DisplayOrder="3" Id="M-00FA_A-0102-03-0000_PT-Mode_EN-3" />
      </TypeRestriction></ParameterType>
    </ParameterTypes>
    <Parameters>
      <Parameter Id="M-00FA_A-0102-03-0000_P-1" Name="level" ParameterType="M-00FA_A-0102-03-0000_PT-U8" Text="Level" SuffixText="%" Access="ReadWrite" Value="5">
        <Memory CodeSegment="M-00FA_A-0102-03-0000_AS-0200" Offset="11" BitOffset="0" />
      </Parameter>
      <Parameter Id="M-00FA_A-0102-03-0000_P-9" Name="mode" ParameterType="M-00FA_A-0102-03-0000_PT-Mode" Text="Mode &gt; all" Value="1" />
      <Parameter Id="M-00FA_A-0102-03-0000_P-6" Name="hardware" ParameterType="M-00FA_A-0102-03-0000_PT-U8" Text="Hardware revision" Access="Read" Value="7" />
      <Parameter Id="M-00FA_A-0102-03-0000_P-7" Name="internal" ParameterType="M-00FA_A-0102-03-0000_PT-U8" Text="Internal" Access="None" Value="1" />
      <Union SizeInBit="8">
        <Memory CodeSegment="M-00FA_A-0102-03-0000_AS-0200" Offset="12" BitOffset="0" />
        <Parameter Id="M-00FA_A-0102-03-0000_UP-2" Name="high" ParameterType="M-00FA_A-0102-03-0000_PT-Nibble" Offset="0" BitOffset="0" Text="High" Value="9" DefaultUnionParameter="true" />
        <Parameter Id="M-00FA_A-0102-03-0000_UP-3" Name="low" ParameterType="M-00FA_A-0102-03-0000_PT-Nibble" Offset="0" BitOffset="4" Text="Low" Value="3" />
      </Union>
      <Parameter Id="M-00FA_A-0102-03-0000_P-4" Name="spare" ParameterType="M-00FA_A-0102-03-0000_PT-U8" Text="Spare" Value="200">
        <Memory CodeSegment="M-00FA_A-0102-03-0000_AS-0200" Offset="14" BitOffset="0" />
      </Parameter>
      <Parameter Id="M-00FA_A-0102-03-0000_P-5" Name="page" ParameterType="M-00FA_A-0102-03-0000_PT-Page" Text="" Value="" />
    </Parameters>
    <ParameterRefs>
      <ParameterRef Id="M-00FA_A-0102-03-0000_P-1_R-1" RefId="M-00FA_A-0102-03-0000_P-1" Value="6" />
      <ParameterRef Id="M-00FA_A-0102-03-0000_P-9_R-9" RefId="M-00FA_A-0102-03-0000_P-9" />
      <ParameterRef Id="M-00FA_A-0102-03-0000_UP-2_R-2" RefId="M-00FA_A-0102-03-0000_UP-2" />
      <ParameterRef Id="M-00FA_A-0102-03-0000_UP-3_R-3" RefId="M-00FA_A-0102-03-0000_UP-3" />
      <ParameterRef Id="M-00FA_A-0102-03-0000_P-4_R-4" RefId="M-00FA_A-0102-03-0000_P-4" />
      <ParameterRef Id="M-00FA_A-0102-03-0000_P-5_R-5" RefId="M-00FA_A-0102-03-0000_P-5" />
      <ParameterRef Id="M-00FA_A-0102-03-0000_P-6_R-6" RefId="M-00FA_A-0102-03-0000_P-6" />
      <ParameterRef Id="M-00FA_A-0102-03-0000_P-7_R-7" RefId="M-00FA_A-0102-03-0000_P-7" />
    </ParameterRefs>
    <ComObjectTable CodeSegment="M-00FA_A-0102-03-0000_AS-0200" Offset="0">
      <ComObject Id="M-00FA_A-0102-03-0000_O-0" Name="o0" Text="Channel A" Number="0" FunctionText="Switch" DatapointType="DPST-1-1" ObjectSize="1 Bit" ReadFlag="Disabled" WriteFlag="Enabled" CommunicationFlag="Enabled" TransmitFlag="Disabled" UpdateFlag="Disabled" ReadOnInitFlag="Disabled" />
      <ComObject Id="M-00FA_A-0102-03-0000_O-1" Name="o1" Text="Channel A" Number="1" FunctionText="Brightness value" DatapointType="DPST-5-1" ObjectSize="1 Byte" ReadFlag="Enabled" WriteFlag="Disabled" CommunicationFlag="Enabled" TransmitFlag="Enabled" UpdateFlag="Disabled" ReadOnInitFlag="Disabled" />
      <ComObject Id="M-00FA_A-0102-03-0000_O-2" Name="o2" Text="Channel A" Number="2" FunctionText="Dimming" DatapointType="DPST-3-7" ObjectSize="4 Bit" ReadFlag="Enabled" WriteFlag="Enabled" CommunicationFlag="Enabled" TransmitFlag="Disabled" UpdateFlag="Disabled" ReadOnInitFlag="Disabled" />
    </ComObjectTable>
    <ComObjectRefs>
      <ComObjectRef Id="M-00FA_A-0102-03-0000_O-0_R-1" RefId="M-00FA_A-0102-03-0000_O-0" />
      <ComObjectRef Id="M-00FA_A-0102-03-0000_O-1_R-2" RefId="M-00FA_A-0102-03-0000_O-1" />
      <ComObjectRef Id="M-00FA_A-0102-03-0000_O-2_R-3" RefId="M-00FA_A-0102-03-0000_O-2" />
    </ComObjectRefs>
    <AddressTable CodeSegment="M-00FA_A-0102-03-0000_AS-0116" Offset="0" MaxEntries="6" />
    <AssociationTable CodeSegment="M-00FA_A-0102-03-0000_AS-0126" Offset="0" MaxEntries="5" />
    <LoadProcedures>
      <LoadProcedure>
        <LdCtrlConnect />
        <LdCtrlWriteProp ObjIdx="0" PropId="8" Verify="false" InlineData="00000000000000000000" />
        <LdCtrlUnload LsmIdx="1" />
        <LdCtrlUnload LsmIdx="2" />
        <LdCtrlUnload LsmIdx="3" />
        <LdCtrlLoad LsmIdx="1" />
        <LdCtrlAbsSegment LsmIdx="1" SegType="0" Address="278" Size="16" Access="49" MemType="3" SegFlags="128" />
        <LdCtrlTaskSegment LsmIdx="1" Address="278" />
        <LdCtrlLoadCompleted LsmIdx="1" />
        <LdCtrlLoad LsmIdx="2" />
        <LdCtrlAbsSegment LsmIdx="2" SegType="0" Address="294" Size="12" Access="49" MemType="3" SegFlags="128" />
        <LdCtrlTaskSegment LsmIdx="2" Address="294" />
        <LdCtrlLoadCompleted LsmIdx="2" />
        <LdCtrlLoad LsmIdx="3" />
        <LdCtrlAbsSegment LsmIdx="3" SegType="0" Address="256" Size="22" Access="48" MemType="3" SegFlags="0" />
        <LdCtrlAbsSegment LsmIdx="3" SegType="0" Address="512" Size="16" Access="48" MemType="3" SegFlags="128" />
        <LdCtrlAbsSegment LsmIdx="3" SegType="0" Address="3000" Size="10" Access="48" MemType="1" SegFlags="0" />
        <LdCtrlTaskSegment LsmIdx="3" Address="512" />
        <LdCtrlTaskPtr LsmIdx="3" InitPtr="513" SavePtr="514" SerialPtr="0" />
        <LdCtrlTaskCtrl1 LsmIdx="3" Address="515" Count="1" />
        <LdCtrlTaskCtrl2 LsmIdx="3" Callback="4660" Address="512" Seg0="3000" Seg1="0" />
        <LdCtrlLoadCompleted LsmIdx="3" />
        <LdCtrlRestart />
        <LdCtrlDisconnect />
      </LoadProcedure>
    </LoadProcedures>
    <Options />
  </Static>
  <Dynamic>
    <Channel Name="main" Text="Main" Id="M-00FA_A-0102-03-0000_CH-1">
      <ParameterBlock Id="M-00FA_A-0102-03-0000_PB-1" Name="general" Text="General">
        <ParameterRefRef RefId="M-00FA_A-0102-03-0000_P-5_R-5" />
        <ParameterRefRef RefId="M-00FA_A-0102-03-0000_P-1_R-1" />
        <ParameterRefRef RefId="M-00FA_A-0102-03-0000_P-9_R-9" />
        <ParameterRefRef RefId="M-00FA_A-0102-03-0000_P-6_R-6" />
        <ParameterRefRef RefId="M-00FA_A-0102-03-0000_P-7_R-7" />
        <ParameterSeparator Id="M-00FA_A-0102-03-0000_PS-1" Text="What follows depends on the mode." />
        <ComObjectRefRef RefId="M-00FA_A-0102-03-0000_O-0_R-1" />
        <ComObjectRefRef RefId="M-00FA_A-0102-03-0000_O-1_R-2" />
        <choose ParamRefId="M-00FA_A-0102-03-0000_P-9_R-9">
          <when test="1">
            <ComObjectRefRef RefId="M-00FA_A-0102-03-0000_O-2_R-3" />
            <ParameterRefRef RefId="M-00FA_A-0102-03-0000_UP-3_R-3" />
          </when>
          <when test="2 3">
            <ParameterRefRef RefId="M-00FA_A-0102-03-0000_UP-2_R-2" />
          </when>
          <when default="true">
            <ParameterRefRef RefId="M-00FA_A-0102-03-0000_P-4_R-4" />
          </when>
        </choose>
      </ParameterBlock>
    </Channel>
  </Dynamic>
</ApplicationProgram>
</ApplicationPrograms>
<Languages>
  <Language Identifier="sv-SE">
    <TranslationUnit RefId="M-00FA_A-0102-03-0000">
      <TranslationElement RefId="M-00FA_A-0102-03-0000_P-1"><Translation AttributeName="Text" Text="Nivå" /></TranslationElement>
      <TranslationElement RefId="M-00FA_A-0102-03-0000_PT-Mode_EN-1"><Translation AttributeName="Text" Text="Koppling" /></TranslationElement>
      <TranslationElement RefId="M-00FA_A-0102-03-0000_PB-1"><Translation AttributeName="Text" Text="Allmänt" /></TranslationElement>
      <TranslationElement RefId="M-00FA_A-0102-03-0000_O-0"><Translation AttributeName="Text" Text="Kanal A" /></TranslationElement>
    </TranslationUnit>
  </Language>
</Languages>
</Manufacturer></ManufacturerData></KNX>"#;

/// A device of [`BCU2_PRODUCT`]: mode 2 selected; object 0 linked to GA-2, object 1 to GA-1.
pub const BCU2_DEVICE: &str = r#"<DeviceInstance Id="P-0001-0_DI-1" Address="9" ApplicationProgramLoaded="true">
  <ParameterInstanceRefs>
    <ParameterInstanceRef RefId="M-00FA_A-0102-03-0000_P-9_R-9" Value="2" />
  </ParameterInstanceRefs>
  <ComObjectInstanceRefs>
    <ComObjectInstanceRef RefId="O-0_R-1" Links="GA-2" />
    <ComObjectInstanceRef RefId="O-1_R-2" Links="GA-1" />
  </ComObjectInstanceRefs>
</DeviceInstance>"#;

/// The group addresses [`BCU2_DEVICE`] links, as `GA-<n> <address>` lines.
pub const BCU2_ADDRESSES: &str = "GA-1 2305\nGA-2 2562\n";

/// Application program for mask 0701 (BIM M112): fixed table places, load control through
/// memory, memory read before it is written, a comparison of the hardware type as a step.
pub const M112_PRODUCT: &str = r#"<KNX><ApplicationProgram Id="M-00FA_A-0201-01-0000" ApplicationNumber="513"
    ApplicationVersion="1" MaskVersion="MV-0701" LoadProcedureStyle="ProductProcedure" PeiType="0"
    DynamicTableManagement="0" MinEtsVersion="4.0">
  <Static>
    <Code>
      <AbsoluteSegment Id="M-00FA_A-0201-01-0000_AS-4000" Address="16384" Size="9" UserMemory="0">
        <Data>AQAA////////</Data><Mask>/wAA////////</Mask>
      </AbsoluteSegment>
      <AbsoluteSegment Id="M-00FA_A-0201-01-0000_AS-4010" Address="16400" Size="7" UserMemory="0">
        <Data>AP///////w==</Data>
      </AbsoluteSegment>
      <AbsoluteSegment Id="M-00FA_A-0201-01-0000_AS-4400" Address="17408" Size="20" UserMemory="0">
        <Data>AgEAARD/BwERVwAAAAAAAAChoqM=</Data>
      </AbsoluteSegment>
    </Code>
    <ParameterTypes>
      <ParameterType Id="M-00FA_A-0201-01-0000_PT-U16"><TypeNumber SizeInBit="16" Type="unsignedInt" /></ParameterType>
      <ParameterType Id="M-00FA_A-0201-01-0000_PT-Text"><TypeText SizeInBit="24" /></ParameterType>
      <ParameterType Id="M-00FA_A-0201-01-0000_PT-S8"><TypeNumber SizeInBit="8" Type="signedInt" /></ParameterType>
    </ParameterTypes>
    <Parameters>
      <Parameter Id="M-00FA_A-0201-01-0000_P-1" ParameterType="M-00FA_A-0201-01-0000_PT-U16" Value="1000">
        <Memory CodeSegment="M-00FA_A-0201-01-0000_AS-4400" Offset="11" BitOffset="0" />
      </Parameter>
      <Parameter Id="M-00FA_A-0201-01-0000_P-2" ParameterType="M-00FA_A-0201-01-0000_PT-Text" Value="Hi">
        <Memory CodeSegment="M-00FA_A-0201-01-0000_AS-4400" Offset="13" BitOffset="0" />
      </Parameter>
      <Parameter Id="M-00FA_A-0201-01-0000_P-3" ParameterType="M-00FA_A-0201-01-0000_PT-S8" Value="-2">
        <Memory CodeSegment="M-00FA_A-0201-01-0000_AS-4400" Offset="16" BitOffset="0" />
      </Parameter>
    </Parameters>
    <ParameterRefs>
      <ParameterRef Id="M-00FA_A-0201-01-0000_P-1_R-1" RefId="M-00FA_A-0201-01-0000_P-1" />
      <ParameterRef Id="M-00FA_A-0201-01-0000_P-2_R-2" RefId="M-00FA_A-0201-01-0000_P-2" />
      <ParameterRef Id="M-00FA_A-0201-01-0000_P-3_R-3" RefId="M-00FA_A-0201-01-0000_P-3" />
    </ParameterRefs>
    <ComObjectTable CodeSegment="M-00FA_A-0201-01-0000_AS-4400" Offset="0">
      <ComObject Id="M-00FA_A-0201-01-0000_O-0" Number="0" ObjectSize="1 Byte" Priority="High" CommunicationFlag="Enabled" ReadFlag="Enabled" ReadOnInitFlag="Disabled" TransmitFlag="Enabled" UpdateFlag="Disabled" WriteFlag="Disabled" />
      <ComObject Id="M-00FA_A-0201-01-0000_O-1" Number="1" ObjectSize="1 Bit" Priority="Low" CommunicationFlag="Enabled" ReadFlag="Disabled" ReadOnInitFlag="Disabled" TransmitFlag="Disabled" UpdateFlag="Disabled" WriteFlag="Enabled" />
    </ComObjectTable>
    <ComObjectRefs>
      <ComObjectRef Id="M-00FA_A-0201-01-0000_O-0_R-1" RefId="M-00FA_A-0201-01-0000_O-0" />
      <ComObjectRef Id="M-00FA_A-0201-01-0000_O-1_R-2" RefId="M-00FA_A-0201-01-0000_O-1" />
    </ComObjectRefs>
    <AddressTable CodeSegment="M-00FA_A-0201-01-0000_AS-4000" Offset="0" MaxEntries="3" />
    <AssociationTable CodeSegment="M-00FA_A-0201-01-0000_AS-4010" Offset="0" MaxEntries="3" />
    <LoadProcedures>
      <LoadProcedure>
        <LdCtrlConnect />
        <LdCtrlCompareProp ObjIdx="0" PropId="78" InlineData="0000000000AB00000000" />
        <LdCtrlUnload LsmIdx="1" />
        <LdCtrlUnload LsmIdx="2" />
        <LdCtrlUnload LsmIdx="3" />
        <LdCtrlLoad LsmIdx="1" />
        <LdCtrlAbsSegment LsmIdx="1" SegType="0" Address="16384" Size="9" Access="255" MemType="3" SegFlags="128" />
        <LdCtrlTaskSegment LsmIdx="1" Address="16384" />
        <LdCtrlLoadCompleted LsmIdx="1" />
        <LdCtrlLoad LsmIdx="2" />
        <LdCtrlAbsSegment LsmIdx="2" SegType="0" Address="16400" Size="7" Access="255" MemType="3" SegFlags="128" />
        <LdCtrlTaskSegment LsmIdx="2" Address="16400" />
        <LdCtrlLoadCompleted LsmIdx="2" />
        <LdCtrlLoad LsmIdx="3" />
        <LdCtrlAbsSegment LsmIdx="3" SegType="0" Address="17408" Size="20" Access="255" MemType="3" SegFlags="128" />
        <LdCtrlTaskSegment LsmIdx="3" Address="17408" />
        <LdCtrlLoadCompleted LsmIdx="3" />
        <LdCtrlRestart />
        <LdCtrlDisconnect />
      </LoadProcedure>
    </LoadProcedures>
    <Options PreferPartialDownloadIfApplicationLoaded="0" TextParameterZeroTerminate="0" ParameterByteOrder="BigEndian" DownloadInvisibleParameters="Background" />
  </Static>
  <Dynamic>
    <Channel><ParameterBlock>
      <ParameterRefRef RefId="M-00FA_A-0201-01-0000_P-1_R-1" />
      <ParameterRefRef RefId="M-00FA_A-0201-01-0000_P-2_R-2" />
      <ParameterRefRef RefId="M-00FA_A-0201-01-0000_P-3_R-3" />
      <ComObjectRefRef RefId="M-00FA_A-0201-01-0000_O-0_R-1" />
    </ParameterBlock></Channel>
  </Dynamic>
</ApplicationProgram></KNX>"#;

/// A device of [`M112_PRODUCT`]: one changed parameter, object 0 linked to GA-7 (address
/// 4660).
pub const M112_DEVICE: &str = r#"<DeviceInstance Address="30" ApplicationProgramLoaded="true">
  <ParameterInstanceRefs>
    <ParameterInstanceRef RefId="M-00FA_A-0201-01-0000_P-1_R-1" Value="4660" />
  </ParameterInstanceRefs>
  <ComObjectInstanceRefs>
    <ComObjectInstanceRef RefId="O-0_R-1" Links="P-0001-0_GA-7" />
  </ComObjectInstanceRefs>
</DeviceInstance>"#;

/// Application program for mask 0705 (BIM M112) that prefers a partial download: dynamic
/// tables, load control through properties, verify mode, a checksum table.
pub const PARTIAL_PRODUCT: &str = r#"<KNX><ApplicationProgram Id="M-00FA_A-0301-01-0000" ApplicationNumber="769"
    ApplicationVersion="1" MaskVersion="MV-0705" LoadProcedureStyle="ProductProcedure" PeiType="0"
    DynamicTableManagement="1">
  <Static>
    <Code>
      <AbsoluteSegment Id="M-00FA_A-0301-01-0000_AS-0700" Address="1792" Size="16" UserMemory="0" />
      <AbsoluteSegment Id="M-00FA_A-0301-01-0000_AS-4000" Address="16384" Size="11" UserMemory="0">
        <Data>AQAA//////////8=</Data><Mask>/wAA//////////8=</Mask>
      </AbsoluteSegment>
      <AbsoluteSegment Id="M-00FA_A-0301-01-0000_AS-400B" Address="16395" Size="9" UserMemory="0">
        <Data>AP//////////</Data><Mask>////////////</Mask>
      </AbsoluteSegment>
      <AbsoluteSegment Id="M-00FA_A-0301-01-0000_AS-4100" Address="16640" Size="12" UserMemory="0">
        <Data>AQCAAIHfx+4BAgME</Data><Mask>////////////////</Mask>
      </AbsoluteSegment>
    </Code>
    <ParameterTypes>
      <ParameterType Id="M-00FA_A-0301-01-0000_PT-U8"><TypeNumber SizeInBit="8" Type="unsignedInt" /></ParameterType>
      <ParameterType Id="M-00FA_A-0301-01-0000_PT-N"><TypeRestriction Base="Value" SizeInBit="4" /></ParameterType>
    </ParameterTypes>
    <Parameters>
      <Parameter Id="M-00FA_A-0301-01-0000_P-1" ParameterType="M-00FA_A-0301-01-0000_PT-U8" Value="85">
        <Memory CodeSegment="M-00FA_A-0301-01-0000_AS-4100" Offset="8" BitOffset="0" />
      </Parameter>
      <Parameter Id="M-00FA_A-0301-01-0000_P-2" ParameterType="M-00FA_A-0301-01-0000_PT-U8" Value="99">
        <Memory CodeSegment="M-00FA_A-0301-01-0000_AS-4100" Offset="9" BitOffset="0" />
      </Parameter>
      <Parameter Id="M-00FA_A-0301-01-0000_P-3" ParameterType="M-00FA_A-0301-01-0000_PT-N" Value="10">
        <Memory CodeSegment="M-00FA_A-0301-01-0000_AS-4100" Offset="11" BitOffset="4" />
      </Parameter>
    </Parameters>
    <ParameterRefs>
      <ParameterRef Id="M-00FA_A-0301-01-0000_P-1_R-1" RefId="M-00FA_A-0301-01-0000_P-1" />
      <ParameterRef Id="M-00FA_A-0301-01-0000_P-2_R-2" RefId="M-00FA_A-0301-01-0000_P-2" />
      <ParameterRef Id="M-00FA_A-0301-01-0000_P-3_R-3" RefId="M-00FA_A-0301-01-0000_P-3" />
    </ParameterRefs>
    <ComObjectTable CodeSegment="M-00FA_A-0301-01-0000_AS-4100" Offset="0">
      <ComObject Id="M-00FA_A-0301-01-0000_O-0" Number="0" ObjectSize="2 Bytes" Priority="Low" CommunicationFlag="Enabled" ReadFlag="Disabled" ReadOnInitFlag="Disabled" TransmitFlag="Enabled" UpdateFlag="Disabled" WriteFlag="Disabled" />
    </ComObjectTable>
    <ComObjectRefs>
      <ComObjectRef Id="M-00FA_A-0301-01-0000_O-0_R-1" RefId="M-00FA_A-0301-01-0000_O-0" ObjectSize="1 Bit" />
    </ComObjectRefs>
    <AddressTable CodeSegment="M-00FA_A-0301-01-0000_AS-4000" Offset="0" MaxEntries="4" />
    <AssociationTable CodeSegment="M-00FA_A-0301-01-0000_AS-400B" Offset="0" MaxEntries="4" />
    <LoadProcedures>
      <LoadProcedure>
        <LdCtrlConnect AppliesTo="auto" />
        <LdCtrlUnload LsmIdx="1" Occurrence="0" AppliesTo="auto" />
        <LdCtrlUnload LsmIdx="2" Occurrence="0" AppliesTo="auto" />
        <LdCtrlUnload LsmIdx="3" Occurrence="0" AppliesTo="auto" />
        <LdCtrlLoad LsmIdx="1" Occurrence="0" AppliesTo="auto" />
        <LdCtrlAbsSegment LsmIdx="1" SegType="0" Address="16384" Size="11" Access="242" MemType="3" SegFlags="128" Occurrence="0" AppliesTo="auto" />
        <LdCtrlTaskSegment LsmIdx="1" Address="16384" Occurrence="0" AppliesTo="auto" />
        <LdCtrlLoadCompleted LsmIdx="1" Occurrence="0" AppliesTo="auto" />
        <LdCtrlLoad LsmIdx="2" Occurrence="0" AppliesTo="auto" />
        <LdCtrlAbsSegment LsmIdx="2" SegType="0" Address="16395" Size="9" Access="242" MemType="3" SegFlags="128" Occurrence="0" AppliesTo="auto" />
        <LdCtrlTaskSegment LsmIdx="2" Address="16395" Occurrence="0" AppliesTo="auto" />
        <LdCtrlLoadCompleted LsmIdx="2" Occurrence="0" AppliesTo="auto" />
        <LdCtrlLoad LsmIdx="3" Occurrence="0" AppliesTo="auto" />
        <LdCtrlAbsSegment LsmIdx="3" SegType="0" Address="1792" Size="16" Access="242" MemType="2" SegFlags="0" Occurrence="0" AppliesTo="auto" />
        <LdCtrlAbsSegment LsmIdx="3" SegType="0" Address="16640" Size="12" Access="243" MemType="3" SegFlags="128" Occurrence="0" AppliesTo="auto" />
        <LdCtrlTaskSegment LsmIdx="3" Address="16640" Occurrence="0" AppliesTo="auto" />
        <LdCtrlTaskCtrl1 LsmIdx="3" Address="0" Count="0" Occurrence="0" AppliesTo="auto" />
        <LdCtrlLoadCompleted LsmIdx="3" Occurrence="0" AppliesTo="auto" />
        <LdCtrlLoadImageProp ObjIdx="3" PropId="27" Count="2" Occurrence="0" StartElement="1" AppliesTo="auto" />
        <LdCtrlRestart AppliesTo="auto" />
        <LdCtrlDisconnect AppliesTo="auto" />
      </LoadProcedure>
    </LoadProcedures>
    <Options PreferPartialDownloadIfApplicationLoaded="1" />
  </Static>
  <Dynamic>
    <ParameterBlock>
      <ParameterRefRef RefId="M-00FA_A-0301-01-0000_P-1_R-1" />
      <ParameterRefRef RefId="M-00FA_A-0301-01-0000_P-3_R-3" />
      <ComObjectRefRef RefId="M-00FA_A-0301-01-0000_O-0_R-1" />
    </ParameterBlock>
  </Dynamic>
</ApplicationProgram></KNX>"#;

/// A loaded device of [`PARTIAL_PRODUCT`] with checksums from its last download for object 3
/// (two checksummed blocks, 0x1234 and 0xBEEF); object 0 linked to GA-5 (address 2049).
pub const PARTIAL_DEVICE: &str = r#"<DeviceInstance Address="110" ApplicationProgramLoaded="true"
    CheckSums="AQABAAEAAAABAAAAAwIAAAABNBIAAAHvvgAA" LoadedImage="AA==">
  <ComObjectInstanceRefs>
    <ComObjectInstanceRef RefId="O-0_R-1" Links="GA-5" />
  </ComObjectInstanceRefs>
</DeviceInstance>"#;

/// Identifier of the made-up project.
pub const PROJECT_ID: &str = "P-0001";
/// Name of the made-up project.
pub const PROJECT_NAME: &str = "Demo house";

/// The devices of the made-up project: (identifier, individual address, whether the planner
/// supports it). All sit on line 1.1; the last one has no address.
pub const DEVICES: [(&str, Option<&str>, bool); 8] = [
    ("P-0001-0_DI-1", Some("1.1.9"), true),
    ("P-0001-0_DI-2", Some("1.1.30"), true),
    ("P-0001-0_DI-3", Some("1.1.110"), true),
    ("P-0001-0_DI-4", Some("1.1.111"), false),
    ("P-0001-0_DI-5", Some("1.1.31"), false),
    ("P-0001-0_DI-6", Some("1.1.74"), false),
    ("P-0001-0_DI-7", Some("1.1.75"), false),
    ("P-0001-0_DI-8", None, false),
];

/// A device element as the project file has it: `element` with identifier, name and the
/// references to its product and application added.
fn place(element: &str, number: u8, name: &str, hardware: &str, program: &str) -> String {
    let bare = element.replacen(r#" Id="P-0001-0_DI-1""#, "", 1);
    bare.replacen(
        "<DeviceInstance",
        &format!(
            r#"<DeviceInstance Id="P-0001-0_DI-{number}" Name="{name}" ProductRefId="M-00FA_H-{hardware}_P-{hardware}" Hardware2ProgramRefId="M-00FA_H-{hardware}_HP-{program}""#
        ),
        1,
    )
}

/// One entry of the hardware file: a product and the application programs it runs.
fn hardware(id: &str, text: &str, attributes: &str, programs: &[&str]) -> String {
    let mut out = format!(
        r#"<Hardware Id="M-00FA_H-{id}" Name="{text}" {attributes}>
             <Products><Product Id="M-00FA_H-{id}_P-{id}" Text="{text}" OrderNumber="{id}" /></Products>
             <Hardware2Programs>"#
    );
    for program in programs {
        out.push_str(&format!(
            r#"<Hardware2Program Id="M-00FA_H-{id}_HP-{program}"><ApplicationProgramRef RefId="M-00FA_A-{program}" /></Hardware2Program>"#
        ));
    }
    if programs.is_empty() {
        out.push_str(&format!(
            r#"<Hardware2Program Id="M-00FA_H-{id}_HP-none" />"#
        ));
    }
    out.push_str("</Hardware2Programs></Hardware>");
    out
}

/// The files of the made-up project, by path, as a project archive holds them: the project
/// directory, one manufacturer directory with its hardware file and application programs,
/// and a piece of baggage that no reader should touch.
///
/// Devices 1 to 3 use the three application programs of this module. Device 4 has a dialog
/// plug-in, device 5 a download plug-in, device 6 a mask without rules and a procedure taken
/// from the mask, the application file of device 7 is missing, and device 8 is a product
/// without application program and without address.
pub fn files() -> Vec<(String, Vec<u8>)> {
    let named = |product: &str, name: &str| {
        product.replacen(
            "<ApplicationProgram ",
            &format!(r#"<ApplicationProgram Name="{name}" "#),
            1,
        )
    };
    // Variants of the supported programs that the planner must refuse.
    let dialog_plugin = PARTIAL_PRODUCT
        .replace("M-00FA_A-0301-01-0000", "M-00FA_A-0302-01-0000")
        .replacen(
            "<Static>",
            r#"<Static><Extension EtsUiPlugin="{00000000-0000-0000-0000-0000000000AA}" />"#,
            1,
        );
    let download_plugin = M112_PRODUCT
        .replace("M-00FA_A-0201-01-0000", "M-00FA_A-0202-01-0000")
        .replacen(
            "<Static>",
            r#"<Static><Extension EtsDownloadPlugin="{00000000-0000-0000-0000-0000000000BB}" />"#,
            1,
        );
    let other_mask = r#"<KNX><ApplicationProgram Id="M-00FA_A-0401-01-0000" Name="Demo gateway"
        ApplicationNumber="1025" ApplicationVersion="1" MaskVersion="MV-07B0"
        LoadProcedureStyle="MergedProcedure"><Static /></ApplicationProgram></KNX>"#;

    let devices = [
        place(BCU2_DEVICE, 1, "Hall dimmer", "D-100", "0102-03-0000"),
        place(M112_DEVICE, 2, "Kitchen actuator", "A-200", "0201-01-0000"),
        place(PARTIAL_DEVICE, 3, "Living sensor", "S-300", "0301-01-0000"),
        place(
            r#"<DeviceInstance Address="111" />"#,
            4,
            "Sensor with dialog plug-in",
            "S-310",
            "0302-01-0000",
        ),
        place(
            r#"<DeviceInstance Address="31" />"#,
            5,
            "Actuator with download plug-in",
            "A-210",
            "0202-01-0000",
        ),
        place(
            r#"<DeviceInstance Address="74" />"#,
            6,
            "Gateway",
            "G-400",
            "0401-01-0000",
        ),
        place(
            r#"<DeviceInstance Address="75" />"#,
            7,
            "Device without its application file",
            "X-900",
            "0999-01-0000",
        ),
        place(r#"<DeviceInstance />"#, 8, "Power supply", "P-500", "none"),
    ]
    .join("\n");
    let installation = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<KNX xmlns="http://knx.org/xml/project/20"><Project Id="{PROJECT_ID}"><Installations>
<Installation Name="" BCUKey="4294967295">
  <Topology>
    <Area Id="P-0001-0_A-1" Address="1" Name="Area">
      <Line Id="P-0001-0_L-1" Address="1" Name="Line" MediumTypeRefId="MT-0">
{devices}
      </Line>
    </Area>
  </Topology>
  <Locations>
    <Space Type="Building" Id="P-0001-0_BP-1" Name="Demo house">
      <Space Type="Floor" Id="P-0001-0_BP-2" Name="Ground floor">
        <Space Type="Room" Id="P-0001-0_BP-3" Name="Hall"><DeviceInstanceRef RefId="P-0001-0_DI-1" /></Space>
        <Space Type="Room" Id="P-0001-0_BP-4" Name="Kitchen">
          <DeviceInstanceRef RefId="P-0001-0_DI-2" /><DeviceInstanceRef RefId="P-0001-0_DI-5" />
        </Space>
        <Space Type="Room" Id="P-0001-0_BP-5" Name="Living room">
          <DeviceInstanceRef RefId="P-0001-0_DI-3" /><DeviceInstanceRef RefId="P-0001-0_DI-4" />
        </Space>
      </Space>
      <Space Type="DistributionBoard" Id="P-0001-0_BP-6" Name="Cabinet">
        <DeviceInstanceRef RefId="P-0001-0_DI-6" /><DeviceInstanceRef RefId="P-0001-0_DI-8" />
      </Space>
    </Space>
  </Locations>
  <GroupAddresses><GroupRanges>
    <GroupRange Id="P-0001-0_GR-1" Name="Lighting" RangeStart="2048" RangeEnd="4095">
      <GroupRange Id="P-0001-0_GR-2" Name="Scenes" RangeStart="2048" RangeEnd="2303">
        <GroupAddress Id="P-0001-0_GA-5" Address="2049" Name="Living scene" DatapointType="DPST-1-1" />
      </GroupRange>
      <GroupRange Id="P-0001-0_GR-3" Name="Switching" RangeStart="2304" RangeEnd="2559">
        <GroupAddress Id="P-0001-0_GA-1" Address="2305" Name="Hall light switch" DatapointType="DPST-1-1" />
        <GroupAddress Id="P-0001-0_GA-8" Address="2306" Name="Hall light brightness" DatapointType="DPST-5-1" />
        <GroupAddress Id="P-0001-0_GA-9" Address="2307" Name="Hall light dimming" DatapointType="DPST-3-7" />
      </GroupRange>
      <GroupRange Id="P-0001-0_GR-4" Name="Status" RangeStart="2560" RangeEnd="2815">
        <GroupAddress Id="P-0001-0_GA-2" Address="2562" Name="Hall light status" DatapointType="DPST-5-1" />
      </GroupRange>
    </GroupRange>
    <GroupRange Id="P-0001-0_GR-5" Name="Heating" RangeStart="4096" RangeEnd="6143">
      <GroupRange Id="P-0001-0_GR-6" Name="Valves" RangeStart="4608" RangeEnd="4863">
        <GroupAddress Id="P-0001-0_GA-7" Address="4660" Name="Kitchen valve" DatapointType="DPST-5-1" />
        <GroupAddress Id="P-0001-0_GA-10" Address="4661" Name="Kitchen temperature" DatapointType="DPST-9-1" />
        <GroupAddress Id="P-0001-0_GA-11" Address="4662" Name="No type yet" />
      </GroupRange>
    </GroupRange>
  </GroupRanges></GroupAddresses>
</Installation></Installations></Project></KNX>"#
    );
    let project = format!(
        r#"<KNX xmlns="http://knx.org/xml/project/20"><Project Id="{PROJECT_ID}">
             <ProjectInformation Name="{PROJECT_NAME}" /></Project></KNX>"#
    );
    let hardware_file = format!(
        r#"<KNX xmlns="http://knx.org/xml/project/20"><ManufacturerData><Manufacturer RefId="M-00FA"><Hardware>
{}{}{}{}{}{}{}{}
</Hardware></Manufacturer></ManufacturerData></KNX>"#,
        hardware("D-100", "Demo dimmer", "", &["0102-03-0000"]),
        hardware("A-200", "Demo actuator", "", &["0201-01-0000"]),
        hardware("S-300", "Demo sensor", "", &["0301-01-0000"]),
        hardware("S-310", "Demo sensor plus", "", &["0302-01-0000"]),
        hardware(
            "A-210",
            "Demo actuator plus",
            r#"NoDownloadWithoutPlugin="1""#,
            &["0202-01-0000"]
        ),
        hardware("G-400", "Demo gateway", "", &["0401-01-0000"]),
        hardware("X-900", "Demo unknown", "", &["0999-01-0000"]),
        hardware("P-500", "Demo power supply", "", &[]),
    );
    let text = |path: &str, content: String| (path.to_string(), content.into_bytes());
    vec![
        text("P-0001/project.xml", project),
        text("P-0001/0.xml", installation),
        text("M-00FA/Hardware.xml", hardware_file),
        text(
            "M-00FA/M-00FA_A-0102-03-0000.xml",
            named(BCU2_PRODUCT, "Demo dimming 0102/0.3"),
        ),
        text(
            "M-00FA/M-00FA_A-0201-01-0000.xml",
            named(M112_PRODUCT, "Demo switching 0201/0.1"),
        ),
        text(
            "M-00FA/M-00FA_A-0301-01-0000.xml",
            named(PARTIAL_PRODUCT, "Demo sensing 0301/0.1"),
        ),
        text(
            "M-00FA/M-00FA_A-0302-01-0000.xml",
            named(&dialog_plugin, "Demo sensing plus 0302/0.1"),
        ),
        text(
            "M-00FA/M-00FA_A-0202-01-0000.xml",
            named(&download_plugin, "Demo switching plus 0202/0.1"),
        ),
        text("M-00FA/M-00FA_A-0401-01-0000.xml", other_mask.to_string()),
        // Baggage: readers must leave it alone.
        (
            "M-00FA/Baggages/plugin.dll".to_string(),
            b"MZ made-up bytes, not a library".to_vec(),
        ),
    ]
}

/// The made-up project as a project archive (`.knxproj`): a ZIP file with stored entries.
pub fn archive() -> Vec<u8> {
    zip::stored(&files())
}
